//! The inspector's Docs tab through the real UI (egui_kittest + AccessKit) on the demo backend's
//! fixture: the tab and its picker, links between docs, http links, Open in editor, the empty
//! state, SSH sessions, and that the demo (whose docs are a fixture) never polls the docs tools.
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

mod common;

use common::*;
use csm_ui::Action;
use csm_ui::model::InspectorTab;
use csm_ui::theme::ThemeKind;
use egui_kittest::Harness;
use egui_kittest::kittest::{NodeT, Queryable};
use serde_json::json;

fn h() -> Harness<'static, csm_ui::CsmApp> {
    let mut h = harness(app_with(backend(ThemeKind::Mocha)));
    settle(&mut h);
    h
}

fn has(h: &Harness<'static, csm_ui::CsmApp>, label: &str) -> bool {
    h.query_by_label_contains(label).is_some()
}

fn show_docs(h: &mut Harness<'static, csm_ui::CsmApp>, session: &str) {
    let ctx = h.ctx.clone();
    h.state_mut().run(&ctx, Action::Tool("session_focus", json!({"session_id": session})));
    h.state_mut().run(&ctx, Action::Tool("inspector_show", json!({"tab": "docs"})));
    settle(h);
    assert_eq!(h.state().model.inspector, Some(InspectorTab::Docs));
    assert_eq!(h.state().model.active.as_deref(), Some(session));
}

fn selected(h: &Harness<'static, csm_ui::CsmApp>, label: &str) -> bool {
    h.get_by_label(label).accesskit_node().toggled() == Some(egui::accesskit::Toggled::True)
}

#[test]
fn the_docs_tab_shows_claude_md_first_and_the_picker_switches() {
    let mut h = h();
    show_docs(&mut h, "web");
    assert!(h.query_by_label("Inspector: Docs").is_some());
    // CLAUDE.md is the default; the picker offers the others.
    for name in ["README.md", "CLAUDE.md", "AGENTS.md"] {
        assert!(h.query_by_label(&format!("Doc: {name}")).is_some(), "{name} in the picker");
    }
    assert!(selected(&h, "Doc: CLAUDE.md"));
    assert!(has(&h, "Storefront for Odulodo"), "CLAUDE.md's text");
    assert!(has(&h, "dotnet build Odulodo.Web.sln"), "its code block");
    // Header: the path, then size and age.
    assert!(h.query_by_label("CLAUDE.md").is_some());
    assert!(has(&h, " · 42m"), "size and age");

    h.get_by_label("Doc: README.md").click();
    settle(&mut h);
    assert!(selected(&h, "Doc: README.md"));
    assert!(has(&h, "Getting started"));
    assert!(!has(&h, "Storefront for Odulodo"));
    // The choice is kept for the folder: another session there shows README.md too.
    show_docs(&mut h, "webps");
    assert!(selected(&h, "Doc: README.md"));
    // The demo's docs are a fixture: nothing is polled.
    assert!(!h.state().submitted().iter().any(|c| c.id.starts_with("project_doc")));
}

#[test]
fn a_link_to_a_sibling_doc_switches_to_it_and_http_links_open() {
    let mut h = h();
    show_docs(&mut h, "web");
    // `[AGENTS.md](AGENTS.md)` in CLAUDE.md.
    h.get_by_label("Link: AGENTS.md").click();
    settle(&mut h);
    assert!(selected(&h, "Doc: AGENTS.md"));
    assert!(has(&h, "These hold for every coding agent"));
    // `[CLAUDE.md](./CLAUDE.md)` back.
    h.get_by_label("Link: CLAUDE.md").click();
    settle(&mut h);
    assert!(selected(&h, "Doc: CLAUDE.md"));
    // An http link goes through `open_url`, with its real target.
    h.get_by_label("Link: Optimizely docs").click();
    settle(&mut h);
    let opened = h.state().submitted().iter().any(|c| c.id == "open_url" && c.params == json!({"url": "https://docs.developers.optimizely.com/commerce-connect"}));
    assert!(opened, "open_url with the link's target");
}

#[test]
fn open_in_editor_dispatches_open_with_and_copy_path_copies() {
    let mut h = h();
    show_docs(&mut h, "web");
    h.get_by_label("Open in editor").click();
    settle(&mut h);
    assert!(h.state().submitted().iter().any(|c| c.id == "open_with" && c.params == json!({"session_id": "web"})));
    assert!(h.query_by_label("Copy path").is_some());
}

#[test]
fn a_folder_without_docs_says_what_was_looked_for() {
    let mut h = h();
    show_docs(&mut h, "craft");
    assert!(has(&h, "No project docs here. Looked for README.md, CLAUDE.md, AGENTS.md, CLAUDE.local.md, GEMINI.md, .claude/CLAUDE.md"));
    assert!(h.query_by_label("Open in editor").is_none());
}

#[test]
fn an_ssh_session_has_none() {
    let mut h = h();
    show_docs(&mut h, "build");
    assert!(has(&h, "an SSH session's folder is on another machine"));
}
