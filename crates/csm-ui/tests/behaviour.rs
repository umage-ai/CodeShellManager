//! Behaviour through the real UI (egui_kittest + AccessKit) on the static demo backend and a real
//! engine: clicks go through the tool registry and come back as a new snapshot.
#![allow(clippy::unwrap_used, clippy::expect_used)]

mod common;

use common::*;
use csm_ui::Action;
use csm_ui::model::{GridPreset, LayoutMode, Scope};
use csm_ui::theme::{ThemeKind, size};
use egui::{Event, Modifiers, PointerButton, Pos2, vec2};
use egui_kittest::Harness;
use egui_kittest::kittest::Queryable;

fn h() -> Harness<'static, csm_ui::CsmApp> {
    let mut h = harness(app_with(backend(ThemeKind::Mocha)));
    settle(&mut h);
    h
}

fn last_call(h: &Harness<'static, csm_ui::CsmApp>) -> (String, serde_json::Value) {
    let c = h.state().submitted().last().cloned().expect("a call was submitted");
    (c.id, c.params)
}

fn has(h: &Harness<'static, csm_ui::CsmApp>, label: &str) -> bool {
    h.query_by_label(label).is_some()
}

fn drag(h: &mut Harness<'static, csm_ui::CsmApp>, from: Pos2, by: egui::Vec2) {
    h.event(Event::PointerMoved(from));
    h.step();
    h.event(Event::PointerButton { pos: from, button: PointerButton::Primary, pressed: true, modifiers: Modifiers::NONE });
    h.step();
    for i in 1..=6 {
        h.event(Event::PointerMoved(from + by * (i as f32 / 6.0)));
        h.step();
    }
    h.event(Event::PointerButton { pos: from + by, button: PointerButton::Primary, pressed: false, modifiers: Modifiers::NONE });
    h.step();
    settle(h);
}

#[test]
fn rail_tab_filters_the_sidebar_and_the_panes() {
    let mut h = h();
    // The demo opens on Odulodo: its sessions in the sidebar and panes, Umage's not.
    assert!(has(&h, "Session: Odulodo.Web"));
    assert!(!has(&h, "Session: CodeShellManager"));
    assert!(has(&h, "Pane: Odulodo"));

    h.get_by_label("Group tab: Umage").click();
    settle(&mut h);
    assert_eq!(last_call(&h).0, "view_set");
    assert_eq!(h.state().model.rail, Scope::Group("umage".into()));
    assert!(has(&h, "Session: CodeShellManager"));
    assert!(has(&h, "Session: OptimizelyNewsletter"), "asleep sessions stay in the list");
    assert!(!has(&h, "Session: Odulodo.Web"), "other groups leave the sidebar");
    assert!(has(&h, "Pane: CodeShellManager"));
    assert!(!has(&h, "Pane: Odulodo"), "the panes follow the tab");

    h.get_by_label("Group tab: All sessions").click();
    settle(&mut h);
    assert!(has(&h, "Session: Odulodo.Web") && has(&h, "Session: CodeShellManager") && has(&h, "Session: api-gateway"));
}

#[test]
fn inbox_closes_reopens_and_its_divider_drags() {
    let mut h = h();
    assert!(has(&h, "Inbox: Odulodo"), "Needs you lists the approval");
    h.get_by_label_contains("Hide the inbox").click();
    settle(&mut h);
    assert_eq!(last_call(&h), ("inbox_panel_set".to_string(), serde_json::json!({"open": false})));
    assert!(!h.state().model.inbox_panel.open);
    assert!(!has(&h, "Inbox: Odulodo"));
    // The badge next to Sessions brings it back.
    h.get_by_label("Show the Needs you inbox").click();
    settle(&mut h);
    assert!(h.state().model.inbox_panel.open);
    assert!(has(&h, "Inbox: Odulodo"));

    // Drag the divider down 60 points: one inbox_panel_set with the new height, at the end.
    let before = h.state().submitted().len();
    let grip = h.get_by_label("Inbox divider").rect().center();
    drag(&mut h, grip, vec2(0.0, 60.0));
    let calls: Vec<_> = h.state().submitted()[before..].iter().filter(|c| c.id == "inbox_panel_set").cloned().collect();
    assert_eq!(calls.len(), 1, "one call when the drag ends, none while it runs: {calls:?}");
    let height = h.state().model.inbox_panel.height;
    assert!((height - (size::INBOX_DEFAULT_H + 60.0)).abs() < 8.0, "height {height}");

    // Double-click resets it.
    let grip = h.get_by_label("Inbox divider").rect().center();
    h.event(Event::PointerMoved(grip));
    h.step();
    // Two clicks, a frame (1/60 s) apart: inside the double-click window.
    for _ in 0..2 {
        h.event(Event::PointerButton { pos: grip, button: PointerButton::Primary, pressed: true, modifiers: Modifiers::NONE });
        h.step();
        h.event(Event::PointerButton { pos: grip, button: PointerButton::Primary, pressed: false, modifiers: Modifiers::NONE });
        h.step();
    }
    settle(&mut h);
    assert_eq!(h.state().model.inbox_panel.height, size::INBOX_DEFAULT_H);
}

fn layout(h: &Harness<'static, csm_ui::CsmApp>) -> csm_ui::model::Layout {
    let m = &h.state().model;
    m.layout(&m.pane_scope())
}

#[test]
fn layout_modes_and_grid_presets() {
    let mut h = h();
    assert_eq!(layout(&h).mode, LayoutMode::Side);
    assert!(has(&h, "Pane: Odulodo.Web (PS)"));

    h.get_by_label("Layout: Focus").click();
    settle(&mut h);
    assert_eq!(layout(&h).mode, LayoutMode::Focus);
    assert_eq!(h.query_all_by_label_contains("Pane: ").count(), 1, "Focus shows one pane");

    h.get_by_label("Layout: Overview").click();
    settle(&mut h);
    assert_eq!(layout(&h).mode, LayoutMode::Overview);
    assert_eq!(h.query_all_by_label_contains("Pane: ").count(), 4, "every live session of the group");

    // Grid opens the presets; pick 3×2.
    h.get_by_label("Layout: Grid").click();
    settle(&mut h);
    assert_eq!(layout(&h).mode, LayoutMode::Grid);
    h.get_by_label("Grid 3×2").click();
    settle(&mut h);
    assert_eq!(layout(&h).grid, GridPreset::new(3, 2));
    let (id, params) = last_call(&h);
    assert_eq!(id, "layout_set");
    assert_eq!(params["grid"], serde_json::json!({"cols": 3, "rows": 2}));
    // Four live sessions in six cells: two empty slots.
    assert_eq!(h.query_all_by_label("Empty slot").count(), 2);

    h.get_by_label("Layout: Free").click();
    settle(&mut h);
    assert_eq!(layout(&h).mode, LayoutMode::Free);
    assert!(h.query_all_by_label("Pane divider").count() >= 2, "the split tree's dividers");
}

#[test]
fn free_mode_divider_drag_persists_the_ratio() {
    let mut h = h();
    h.get_by_label("Layout: Free").click();
    settle(&mut h);
    let before = layout(&h).tree.unwrap();
    // The top end of a divider: the floating pane covers its middle.
    let div = h.get_all_by_label("Pane divider").next().unwrap().rect().center_top() + vec2(0.0, 20.0);
    drag(&mut h, div, vec2(-80.0, 0.0));
    let (id, params) = last_call(&h);
    assert_eq!(id, "layout_set");
    assert!(params.get("tree").is_some());
    let after = layout(&h).tree.unwrap();
    assert_ne!(before, after, "the ratio changed and was stored");
}

#[test]
fn palette_lists_every_registered_tool() {
    let mut h = h();
    let ctx = h.ctx.clone();
    h.state_mut().run(&ctx, Action::OpenPalette);
    settle(&mut h);
    // ">" lists commands only: every tool in the registry (consent_answer is answered on its card).
    let app = h.state();
    let rows = csm_ui::screens::palette::rows(app, ">");
    let listed: Vec<&str> = rows
        .iter()
        .filter_map(|r| match r {
            csm_ui::screens::palette::PalRow::Tool { id, .. } => Some(*id),
            _ => None,
        })
        .collect();
    for spec in app.dispatch.registry().all() {
        if spec.id != "consent_answer" {
            assert!(listed.contains(&spec.id), "{} missing from the palette", spec.id);
        }
    }
    // Typing filters by title, fuzzily, and shows sessions first.
    h.state_mut().ui.palette.query = "sleep".into();
    settle(&mut h);
    assert!(has(&h, "Palette: Sleep session"));
    assert!(has(&h, "Palette: Sleep all in group"));
    h.get_by_label("Palette: Sleep all in group").click();
    settle(&mut h);
    let (id, params) = last_call(&h);
    assert_eq!(id, "group_sleep_all");
    assert_eq!(params["group_id"], "odulodo", "params filled from the group the panes show");
}

#[test]
fn buttons_submit_their_tool() {
    let mut h = h();
    h.get_by_label("Group tab: All sessions").click();
    settle(&mut h);
    h.get_by_label("Session: CodeShellManager-rs").click();
    settle(&mut h);
    assert_eq!(last_call(&h), ("session_focus".to_string(), serde_json::json!({"session_id": "rs"})));
    assert_eq!(h.state().model.active.as_deref(), Some("rs"));

    h.get_by_label("Jump to the next session that needs you").click();
    settle(&mut h);
    assert_eq!(last_call(&h).0, "inbox_next");
    assert_eq!(h.state().model.active.as_deref(), Some("odulodo"), "oldest Needs-you first");

    h.get_by_label("Run build · Odulodo").click();
    settle(&mut h);
    assert_eq!(last_call(&h), ("run_start".to_string(), serde_json::json!({"session_id": "odulodo"})));

    h.get_by_label_contains("Inspector  Ctrl+I").click();
    settle(&mut h);
    assert_eq!(last_call(&h), ("inspector_show".to_string(), serde_json::json!({"tab": "notes"})));
    h.get_by_label("Inspector: Session").click();
    settle(&mut h);
    assert_eq!(h.state().model.inspector, Some(csm_ui::model::InspectorTab::Session));

    h.get_by_label("Group view: layers").click();
    settle(&mut h);
    assert_eq!(last_call(&h), ("view_set".to_string(), serde_json::json!({"mode": "headers"})));
    assert!(has(&h, "Group: Umage"), "headers view lists every group");
}

#[test]
fn consent_request_shows_a_card_and_the_answer_is_a_tool_call() {
    let mut h = h();
    // An MCP client asks to type into a session: the engine parks it and asks the person.
    let engine = h.state().dispatch.engine().clone();
    let caller = csm_engine::Caller::External { client: "claude-code".into(), session_id: Some("rs".into()) };
    let reply = engine.submit(caller, "session_send_text", serde_json::json!({"session_id": "web", "text": "hi"}));
    for _ in 0..50 {
        h.step();
        if has(&h, "Allow once") {
            break;
        }
        std::thread::sleep(std::time::Duration::from_millis(5));
    }
    // A new area is sized invisibly on its first pass; let it settle before clicking.
    for _ in 0..3 {
        h.step();
    }
    assert!(h.query_by_label_contains("claude-code in CodeShellManager-rs wants to run").is_some());
    h.get_by_label("Don't allow").click();
    settle(&mut h);
    settle(&mut h);
    assert_eq!(last_call(&h).0, "consent_answer");
    assert!(matches!(reply.recv_timeout(std::time::Duration::from_secs(5)), Ok(Err(csm_engine::ToolError::Declined(_)))));
    assert!(!has(&h, "Allow once"), "the card goes away");
}

#[test]
fn quick_open_starts_a_session_with_the_cycled_command() {
    let mut h = h();
    let ctx = h.ctx.clone();
    h.state_mut().run(&ctx, Action::OpenQuickOpen { group: None });
    settle(&mut h);
    h.key_press(egui::Key::Tab); // claude → pwsh
    h.step();
    h.get_by_label("Folder: Odulodo.Admin").click();
    settle(&mut h);
    let (id, params) = last_call(&h);
    assert_eq!(id, "session_new");
    assert_eq!(params["folder"], "C:\\GitHub\\Odulodo.Admin");
    assert_eq!(params["command"], "pwsh");
    assert!(has(&h, "Session: Odulodo.Admin (pwsh)"));
}
