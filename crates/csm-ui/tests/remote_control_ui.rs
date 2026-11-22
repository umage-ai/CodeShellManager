//! The Remote Control mark: a session with Claude Code Remote Control on shows it in its sidebar
//! row and its pane header, named with its explanation; clicking it opens the session's link
//! through `open_url`. Sessions without it show none.
#![allow(clippy::unwrap_used, clippy::expect_used)]

mod common;

use common::*;
use csm_ui::theme::ThemeKind;
use csm_ui::widgets::REMOTE_CONTROL_TIP;
use egui_kittest::Harness;
use egui_kittest::kittest::Queryable;
use serde_json::json;

type H = Harness<'static, csm_ui::CsmApp>;

#[test]
fn the_mark_shows_where_remote_control_is_on_and_opens_its_link() {
    let mut h: H = harness(app_with(backend(ThemeKind::Mocha)));
    settle(&mut h);
    let on: Vec<String> = h.state().model.sessions.iter().filter(|s| s.remote_control).map(|s| s.id.clone()).collect();
    assert_eq!(on, ["web"], "the demo has one session with Remote Control on");
    assert_eq!(h.state().model.active.as_deref(), Some("web"));

    // Its sidebar row and its (active) pane header: two marks, both named with the explanation.
    let marks = h.query_all_by_label(REMOTE_CONTROL_TIP).count();
    assert_eq!(marks, 2, "one in the sidebar row, one in the pane header");
    assert_eq!(REMOTE_CONTROL_TIP, "Remote Control is on \u{2014} continue from claude.ai or the Claude app");

    // A click opens the session's claude.ai link (the open_url tool), and does not focus another row.
    let before = h.state().submitted().len();
    h.query_all_by_label(REMOTE_CONTROL_TIP).next().unwrap().click();
    settle(&mut h);
    let opened: Vec<serde_json::Value> = h.state().submitted()[before..].iter().filter(|c| c.id == "open_url").map(|c| c.params.clone()).collect();
    assert_eq!(opened, vec![json!({"url": "https://claude.ai/code/session_01DemoRemoteControlLink"})]);
}

#[test]
fn no_mark_for_a_session_without_remote_control() {
    let mut h: H = harness(app_with(backend(ThemeKind::Mocha)));
    settle(&mut h);
    // Focus a session without it: its pane header has no mark; only the web row keeps one.
    h.get_by_label("Session: Odulodo").click();
    settle(&mut h);
    assert_eq!(h.state().model.active.as_deref(), Some("odulodo"));
    let panes_with_web = h.query_all_by_label("Pane: Odulodo.Web").count();
    assert_eq!(h.query_all_by_label(REMOTE_CONTROL_TIP).count(), 1 + panes_with_web, "the web row, plus the web pane if it is still shown");
}
