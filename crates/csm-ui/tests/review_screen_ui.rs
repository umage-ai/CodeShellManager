//! The Review add-on's real view tree (the plugin builds it; `apps/csm-review/tests/fixture.rs`
//! keeps `fixtures/review_screen.json` equal to it) drawn by the real UI (egui_kittest + AccessKit)
//! on the demo backend: the diff with its inline threads and the agent's mark, the range selection
//! that becomes a `range_action`, the status buttons that run the add-on's own tools as a click of
//! the person, and "Send to agent" asking first with the exact text and the target session.
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

mod common;

use std::sync::{Arc, Mutex};

use common::*;
use csm_core::addons::screen::{EventKind, ScreenScope, ScreenTree};
use csm_engine::{DeclaredScreen, ScreenInfo, ScreenPhase};
use csm_ui::demo::DemoScreens;
use csm_ui::terminal::lock;
use csm_ui::theme::ThemeKind;
use egui_kittest::Harness;
use egui_kittest::kittest::{NodeT, Queryable};
use serde_json::{Value, json};

type H = Harness<'static, csm_ui::CsmApp>;
type Handle = Arc<Mutex<DemoScreens>>;

const FIXTURE: &str = include_str!("fixtures/review_screen.json");

fn tree(rev: u64, root: Value) -> Arc<ScreenTree> {
    Arc::new(ScreenTree::from_render(&json!({"rev": rev, "root": root}), "review", None).unwrap_or_else(|e| panic!("{e}")))
}

fn fixture() -> Value {
    serde_json::from_str(FIXTURE).unwrap()
}

fn app_with_review(root: Value) -> (H, Handle) {
    let b = backend(ThemeKind::Mocha);
    let handle = b.screens_handle();
    {
        let mut g = handle.lock().unwrap();
        g.declared = vec![DeclaredScreen {
            addon: "review".into(),
            addon_name: "Review".into(),
            id: "review.changes".into(),
            title: "Review".into(),
            icon: "layers".into(),
            scope: ScreenScope::Project,
            summary: String::new(),
        }];
        g.instances = vec![ScreenInfo {
            instance: "i1".into(),
            addon: "review".into(),
            addon_name: "Review".into(),
            screen: "review.changes".into(),
            title: "Review".into(),
            icon: "layers".into(),
            scope: ScreenScope::Project,
            phase: ScreenPhase::Ready,
            popped_out: false,
            error: None,
            rev: 1,
            context: Default::default(),
        }];
        g.trees.insert("i1".into(), tree(1, root));
    }
    let mut h = harness(app_with(b));
    settle(&mut h);
    lock(&h.state().ui.screens).active = Some("i1".into());
    settle(&mut h);
    (h, handle)
}

fn calls(h: &H, id: &str) -> Vec<Value> {
    h.state().submitted().iter().filter(|c| c.id == id).map(|c| c.params.clone()).collect()
}

fn has(h: &H, label: &str) -> bool {
    h.query_by_label(label).is_some()
}

fn has_part(h: &H, part: &str) -> bool {
    h.query_by_label_contains(part).is_some()
}

#[test]
fn the_review_screen_draws_the_diff_the_threads_and_the_agent_mark() {
    let (h, _) = app_with_review(fixture());
    assert!(has(&h, "Add-on screen: Review (Review)"), "the frame CSM draws names the add-on");
    // The diff, with both numbers (the new text has `let x = read()?;` on line 2).
    assert!(has(&h, "New line 2") && has(&h, "Old line 2"));
    assert!(has_part(&h, "let x = read()?;"));
    // The first thread is open under line 2, with both comments and its buttons.
    assert!(has_part(&h, "This swallows the error; return it."), "the person's comment");
    assert!(has_part(&h, "Returned it in 1a2b3c."), "the agent's reply");
    assert!(has_part(&h, "Claude in shop"), "the agent is named");
    assert!(has(&h, "Badge: addressed"), "the thread says it is addressed");
    assert!(h.query_all_by_label("Resolve").count() >= 1 && has(&h, "Reopen"), "an addressed thread can be resolved or reopened");
    assert!(h.query_all_by_label("Delete").count() >= 1, "and deleted");
    assert!(has(&h, "Comment on file"), "a whole-file comment is one click away");
    assert!(h.query_all_by_label("Send to agent").count() >= 2, "the toolbar button and the thread's own");
    assert!(has_part(&h, "view only"), "the header says what this is");
    // The thread list on the right names both threads.
    assert!(has(&h, "Item: src/lib.rs:2") && has(&h, "Item: src/lib.rs:3"));
    assert!(has(&h, "File: src/lib.rs") && has(&h, "File: notes.txt"), "the changed files, the untracked one too");
}

#[test]
fn selecting_lines_offers_the_comment_action_and_sends_it() {
    let (mut h, handle) = app_with_review(fixture());
    h.get_by_label("New line 1").click();
    h.step();
    h.get_by_label("New line 2").click_modifiers(egui::Modifiers::SHIFT);
    settle(&mut h);
    assert!(has(&h, "+ Comment"), "the plugin's range action, in a bar CSM draws");
    h.get_by_label("+ Comment").click();
    settle(&mut h);
    let sent: Vec<_> = handle.lock().unwrap().events.iter().filter(|(_, e)| e.kind == EventKind::RangeAction).cloned().collect();
    assert_eq!(sent.len(), 1);
    assert_eq!((sent[0].1.key.as_str(), &sent[0].1.value), ("diff", &json!({"id": "comment", "side": "new", "start": 1, "end": 2})));
}

#[test]
fn a_status_button_runs_the_add_ons_own_tool_as_the_person() {
    let (mut h, _) = app_with_review(fixture());
    h.query_all_by_label("Resolve").next().unwrap().click();
    settle(&mut h);
    let c = calls(&h, "review.set_status");
    assert_eq!(c.len(), 1, "{c:?}");
    assert_eq!(c[0]["status"], "resolved");
    assert_eq!(c[0]["project"], "repo:C:/code/shop");
    assert!(c[0]["thread_id"].as_str().unwrap().starts_with("t_"));
    h.get_by_label("Reopen").click();
    settle(&mut h);
    assert_eq!(calls(&h, "review.set_status")[1]["status"], "open");
    // Delete is a tool too; the add-on refuses it for anyone but the person, and it is the person here.
    h.query_all_by_label("Delete").next().unwrap().click();
    settle(&mut h);
    assert_eq!(calls(&h, "review.delete").len(), 1);
}

#[test]
fn send_to_agent_asks_first_with_the_exact_text_and_the_session() {
    let (mut h, _) = app_with_review(fixture());
    // The first one is the toolbar's (all open threads); the thread's own follows it.
    h.query_all_by_label("Send to agent").next().unwrap().click();
    settle(&mut h);
    assert!(calls(&h, "session_send_text").is_empty(), "nothing is pasted before the answer");
    assert!(h.query_by_label_contains("Review wants to paste text into a session").is_some());
    let body = h.get_by_label_contains("Exact text:").accesskit_node().value().unwrap_or_default();
    assert!(body.contains("Review comments are waiting in CodeShellManager for shop"), "{body}");
    assert!(body.contains("review.reply"), "the prompt names the tools: {body}");
    h.get_by_label("Cancel").click();
    settle(&mut h);
    assert!(calls(&h, "session_send_text").is_empty());
}

#[test]
fn hostile_text_in_a_comment_is_shown_not_obeyed() {
    // The same screen with an escape sequence and a bidi override in the agent's comment.
    let text = fixture().to_string().replace("Returned it in 1a2b3c.", r"Returned \u001b[31m in \u202e1a2b3c.");
    let (h, _) = app_with_review(serde_json::from_str(&text).unwrap());
    assert!(has_part(&h, "U+202E"), "a bidi override is drawn as a marker, in a warning tint");
    assert!(has_part(&h, "Claude in shop"));
}
