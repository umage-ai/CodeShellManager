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
    assert_eq!((params["cols"].as_u64(), params["rows"].as_u64()), (Some(3), Some(2)));
    // Four live sessions in six cells: two empty slots.
    assert_eq!(h.query_all_by_label("Empty slot").count(), 2);

    h.get_by_label("Layout: Free").click();
    settle(&mut h);
    assert_eq!(layout(&h).mode, LayoutMode::Free);
    assert!(h.query_all_by_label("Pane divider").count() >= 2, "the split tree's dividers");
}

#[test]
fn the_grid_menu_lists_the_presets_and_remembers_a_custom_until_forgotten() {
    let mut h = h();
    h.get_by_label("Layout: Grid").click();
    settle(&mut h);
    for p in ["1", "2", "4×1", "6×1", "2×2", "3×2", "6×2", "3×3", "Custom…"] {
        assert!(has(&h, &format!("Grid {p}")), "Grid {p} is offered");
    }
    assert_eq!(h.query_all_by_label_contains("Forget grid").count(), 0, "nothing remembered yet");
    // The new presets apply like the old ones and are not remembered as customs.
    h.get_by_label("Grid 6×2").click();
    settle(&mut h);
    assert_eq!(layout(&h).grid, GridPreset::new(6, 2));
    assert!(h.state().model.custom_grids.is_empty());

    // Custom… 5×1, Apply.
    h.get_by_label("Layout: Grid").click();
    settle(&mut h);
    h.get_by_label("Grid Custom…").click();
    settle(&mut h);
    h.ctx.data_mut(|d| d.insert_temp(egui::Id::new("custom-grid-size"), (5u8, 1u8)));
    h.get_by_label("Apply").click();
    settle(&mut h);
    assert_eq!(layout(&h).grid, GridPreset::new(5, 1));
    assert!(!has(&h, "Grid 5×1"), "the menu closed");

    // Reopened: the custom is offered under the presets, and picking it applies it.
    h.get_by_label("Layout: Grid").click();
    settle(&mut h);
    assert!(has(&h, "Grid 5×1"));
    assert!(has(&h, "Forget grid 5×1"));
    h.get_by_label("Grid 2×2").click();
    settle(&mut h);
    h.get_by_label("Layout: Grid").click();
    settle(&mut h);
    h.get_by_label("Grid 5×1").click();
    settle(&mut h);
    assert_eq!(layout(&h).grid, GridPreset::new(5, 1));

    // Its × forgets it (a tool call), and the menu stays open without it.
    h.get_by_label("Layout: Grid").click();
    settle(&mut h);
    h.get_by_label("Forget grid 5×1").hover();
    settle(&mut h);
    h.get_by_label("Forget grid 5×1").click();
    settle(&mut h);
    assert_eq!(last_call(&h), ("grid_preset_forget".to_string(), serde_json::json!({"cols": 5, "rows": 1})));
    assert!(h.state().model.custom_grids.is_empty());
    assert!(has(&h, "Grid 2×2"), "the menu is still open");
    assert!(!has(&h, "Grid 5×1"));
    assert_eq!(layout(&h).grid, GridPreset::new(5, 1), "forgetting a size leaves the layout as it is");
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
    assert!(params["free"].get("tree").is_some(), "the Free layout goes to the engine as one blob");
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
            csm_ui::screens::palette::PalRow::Tool { id, .. } => Some(id.as_str()),
            _ => None,
        })
        .collect();
    for spec in app.dispatch.tools().iter() {
        if spec.id != "consent_answer" {
            assert!(listed.contains(&spec.id.as_str()), "{} missing from the palette", spec.id);
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

    // Focus gives the pane room for its Run button (Side by side now also keeps the unpinned
    // session used last, CodeShellManager-rs, so five narrow panes share the canvas).
    h.get_by_label("Layout: Focus").click();
    settle(&mut h);
    h.get_by_label("Run build · Odulodo").click();
    settle(&mut h);
    assert_eq!(last_call(&h), ("run_start".to_string(), serde_json::json!({"session_id": "odulodo"})));

    h.get_by_label_contains("Inspector  Ctrl+I").click();
    settle(&mut h);
    // The Notes tab then asks the notes store for the note (`note_get`), so the button's call is not the last one.
    assert!(h.state().submitted().iter().any(|c| c.id == "inspector_show" && c.params == serde_json::json!({"tab": "notes"})));
    assert!(h.state().submitted().iter().any(|c| c.id == "note_get"));
    h.get_by_label("Inspector: Session").click();
    settle(&mut h);
    assert_eq!(h.state().model.inspector, Some(csm_ui::model::InspectorTab::Session));

    h.get_by_label("Group view: layers").click();
    settle(&mut h);
    assert_eq!(last_call(&h), ("view_set".to_string(), serde_json::json!({"view": "headers"})));
    assert!(has(&h, "Group: Umage"), "headers view lists every group");
}

#[test]
fn consent_request_shows_a_card_and_the_answer_is_a_tool_call() {
    let mut h = h();
    // An MCP client asks to type into a session: the engine parks it and asks the person.
    let engine = h.state().dispatch.engine().expect("an engine port");
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
    h.key_press(egui::Key::Tab); // to the folder field's Browse stop
    h.step();
    h.key_press(egui::Key::Tab); // to the command row
    h.step();
    h.key_press(egui::Key::ArrowRight); // claude → pwsh
    h.step();
    h.get_by_label("Folder: Odulodo.Admin").click();
    settle(&mut h);
    let (id, params) = last_call(&h);
    assert_eq!(id, "session_new");
    assert_eq!(params["folder"], "C:\\GitHub\\Odulodo.Admin");
    assert_eq!(params["command"], "pwsh");
    assert!(has(&h, "Session: Odulodo.Admin (pwsh)"));
}

/// An absolute path for this platform that fuzzy-matches the demo's `C:\GitHub\Odulodo.Admin`
/// (on Windows) without being it.
fn near_miss_path() -> &'static str {
    if cfg!(windows) { "C:\\GitHub\\Odulodo.Ad" } else { "/GitHub/Odulodo.Ad" }
}

fn open_quick_and_type(h: &mut Harness<'static, csm_ui::CsmApp>, text: &str) {
    let ctx = h.ctx.clone();
    h.state_mut().run(&ctx, Action::OpenQuickOpen { group: None });
    settle(h);
    h.event(Event::Text(text.into()));
    h.step();
    h.step();
}

#[test]
fn quick_open_starts_in_a_typed_path_not_a_suggestion_it_resembles() {
    let mut h = h();
    open_quick_and_type(&mut h, near_miss_path());
    if cfg!(windows) {
        let rows = csm_ui::screens::palette::folder_rows(h.state(), near_miss_path());
        assert!(!rows.is_empty(), "the near miss matches a suggestion (the case that used to win)");
    }
    h.key_press(egui::Key::Enter);
    settle(&mut h);
    let (id, params) = last_call(&h);
    assert_eq!(id, "session_new");
    assert_eq!(params["folder"], near_miss_path());
}

#[test]
fn quick_open_arrow_from_a_typed_path_picks_the_suggestion() {
    let mut h = h();
    open_quick_and_type(&mut h, near_miss_path());
    let first = csm_ui::screens::palette::folder_rows(h.state(), near_miss_path()).first().map(|(i, _)| h.state().model.folders[*i].path.clone());
    h.key_press(egui::Key::ArrowDown);
    h.step();
    h.key_press(egui::Key::Enter);
    settle(&mut h);
    let (id, params) = last_call(&h);
    assert_eq!(id, "session_new");
    // On Windows the near miss matches a suggestion, and ↓ + Enter starts there; elsewhere nothing
    // matches and the typed path stands.
    assert_eq!(params["folder"], first.unwrap_or_else(|| near_miss_path().to_string()).as_str());
}

#[test]
fn quick_open_browse_asks_the_host_for_a_folder_and_fills_the_field() {
    let mut h = h();
    let ctx = h.ctx.clone();
    h.state_mut().run(&ctx, Action::OpenQuickOpen { group: None });
    settle(&mut h);
    h.get_by_label("Browse…").click();
    h.step();
    assert!(h.state_mut().take_folder_request().is_some(), "Browse… queues a folder dialog for the host");
    assert!(h.state_mut().take_folder_request().is_none(), "once");
    let picked = if cfg!(windows) { "D:\\work\\picked" } else { "/work/picked" };
    h.state_mut().folder_picked(Some(picked.into()));
    h.step();
    assert_eq!(h.state().ui.quick.query, picked);
    h.key_press(egui::Key::Enter);
    settle(&mut h);
    let (id, params) = last_call(&h);
    assert_eq!(id, "session_new");
    assert_eq!(params["folder"], picked);
}

#[test]
fn a_cancelled_folder_dialog_leaves_the_field_alone() {
    let mut h = h();
    open_quick_and_type(&mut h, "odu");
    h.state_mut().folder_picked(None);
    h.step();
    assert_eq!(h.state().ui.quick.query, "odu");
}

#[test]
fn a_pane_tears_off_into_its_own_window_and_docks_back() {
    let mut h = h();
    h.get_by_label("Layout: Free").click();
    settle(&mut h);
    // The pane's ⋯ menu → Move to a new window.
    let ctx = h.ctx.clone();
    h.state_mut().run(&ctx, Action::OpenMenu(csm_ui::app::Menu::Pane { session: "webps".into(), at: egui::pos2(1200.0, 400.0) }));
    for _ in 0..3 {
        h.step();
    }
    h.get_by_label("Move to a new window").click();
    settle(&mut h);
    assert_eq!(last_call(&h).0, "pane_tear_off");
    assert_eq!(layout(&h).torn, vec!["webps".to_string()]);
    // The main window no longer lays it out; its own window (a deferred viewport, embedded as a
    // window under the test harness) shows it, with a dock button instead of tear-off.
    assert!(has(&h, "Pane: Odulodo.Web (PS)"));
    assert!(has(&h, "Dock back into the main window"));
    h.get_by_label("Dock back into the main window").click();
    settle(&mut h);
    assert_eq!(last_call(&h).0, "pane_dock");
    // The dock is applied when its reply lands; the torn-off window is a deferred viewport, so on a
    // slow machine give it a few more frames (bounded) before judging.
    for _ in 0..200 {
        if layout(&h).torn.is_empty() {
            break;
        }
        h.step();
        std::thread::sleep(std::time::Duration::from_millis(5));
    }
    assert!(
        layout(&h).torn.is_empty(),
        "still torn off: {:?}; last calls: {:?}",
        layout(&h).torn,
        h.state().submitted().iter().rev().take(3).map(|c| &c.id).collect::<Vec<_>>()
    );
}

/// One pass of a torn-off window as eframe runs a deferred viewport: its own input (the window's
/// bounds in points at 100%, an optional close), its callback. Returns its viewport commands.
fn torn_pass(h: &Harness<'static, csm_ui::CsmApp>, vid: egui::ViewportId, x: f32, time: f64, close: bool) -> Vec<egui::ViewportCommand> {
    let cb = h.output().viewport_output.get(&vid).and_then(|v| v.viewport_ui_cb.clone()).expect("the torn-off window is a deferred viewport");
    let outer = egui::Rect::from_min_size(egui::pos2(x, 100.0), vec2(976.0, 599.0));
    let info = egui::ViewportInfo {
        native_pixels_per_point: Some(1.0),
        outer_rect: Some(outer),
        inner_rect: Some(egui::Rect::from_min_max(outer.min + vec2(8.0, 31.0), outer.max - vec2(8.0, 8.0))),
        maximized: Some(false),
        minimized: Some(false),
        events: if close { vec![egui::ViewportEvent::Close] } else { vec![] },
        ..Default::default()
    };
    let mut input = egui::RawInput { viewport_id: vid, time: Some(time), ..Default::default() };
    input.viewports.insert(vid, info);
    let out = h.ctx.run_ui(input, |ui| cb(ui));
    // Only the bounds commands (egui adds its own, e.g. the title, on a first pass).
    let cmds = out.viewport_output.get(&vid).map(|v| v.commands.clone()).unwrap_or_default();
    cmds.into_iter()
        .filter(|c| matches!(c, egui::ViewportCommand::OuterPosition(_) | egui::ViewportCommand::InnerSize(_) | egui::ViewportCommand::Maximized(_)))
        .collect()
}

#[test]
fn a_torn_off_windows_bounds_are_saved_as_it_moves_and_restored_when_torn_off_again() {
    let mut h = h();
    h.get_by_label("Layout: Free").click();
    settle(&mut h);
    let ctx = h.ctx.clone();
    h.state_mut().run(&ctx, Action::Tool("pane_tear_off", serde_json::json!({"session_id": "webps"})));
    settle(&mut h);
    assert_eq!(layout(&h).torn, vec!["webps".to_string()]);
    // As in the app: the window is a real OS window (a deferred viewport), not embedded.
    h.ctx.set_embed_viewports(false);
    h.step();
    let vid = egui::ViewportId::from_hash_of(("torn-pane", "webps".to_string()));
    let before = h.state().submitted().len();

    // Nothing saved yet: no restore. Dragged from x=100 to 400: nothing is sent mid-drag.
    assert!(torn_pass(&h, vid, 100.0, 10.0, false).is_empty());
    torn_pass(&h, vid, 250.0, 10.1, false);
    torn_pass(&h, vid, 400.0, 10.2, false);
    h.step();
    assert!(calls_since(&h, before, "window_placement_set").is_empty(), "nothing while it moves");
    // Settled: saved, in physical pixels (100% here), once.
    torn_pass(&h, vid, 400.0, 10.8, false);
    torn_pass(&h, vid, 400.0, 11.5, false);
    settle(&mut h);
    let saved = calls_since(&h, before, "window_placement_set");
    assert_eq!(
        saved,
        vec![serde_json::json!({"window": "pane:webps", "left": 400, "top": 100, "width": 976, "height": 599, "dpi": 96, "maximized": false})]
    );
    assert_eq!(h.state().model.windows["pane:webps"].placement.left, 400);

    // Moved again and closed before it settled: the bounds are saved first, then it docks.
    let before = h.state().submitted().len();
    torn_pass(&h, vid, 520.0, 20.0, false);
    torn_pass(&h, vid, 520.0, 20.05, true);
    settle(&mut h);
    let ids: Vec<&str> =
        h.state().submitted()[before..].iter().map(|c| c.id.as_str()).filter(|id| id.starts_with("window") || id.starts_with("pane")).collect();
    assert_eq!(ids, vec!["window_placement_set", "pane_dock"]);
    assert_eq!(calls_since(&h, before, "window_placement_set")[0]["left"], 520);
    for _ in 0..200 {
        if layout(&h).torn.is_empty() {
            break;
        }
        h.step();
        std::thread::sleep(std::time::Duration::from_millis(5));
    }
    assert!(layout(&h).torn.is_empty());

    // Torn off again: its first frame puts it back where it was.
    let ctx = h.ctx.clone();
    h.state_mut().run(&ctx, Action::Tool("pane_tear_off", serde_json::json!({"session_id": "webps"})));
    settle(&mut h);
    h.step();
    let cmds = torn_pass(&h, vid, 0.0, 30.0, false);
    assert_eq!(cmds.first(), Some(&egui::ViewportCommand::OuterPosition(egui::pos2(520.0, 100.0))));
    assert!(cmds.contains(&egui::ViewportCommand::InnerSize(vec2(960.0, 560.0))), "{cmds:?}");
    assert!(torn_pass(&h, vid, 520.0, 30.1, false).is_empty(), "restored once");
}

#[test]
fn the_main_windows_bounds_are_saved_once_it_settles() {
    let mut h = h();
    let before = h.state().submitted().len();
    {
        let root = h.input_mut().viewports.entry(egui::ViewportId::ROOT).or_default();
        root.outer_rect = Some(egui::Rect::from_min_size(egui::pos2(40.0, 30.0), vec2(1296.0, 839.0)));
        root.inner_rect = Some(egui::Rect::from_min_size(egui::pos2(48.0, 61.0), vec2(1280.0, 800.0)));
        root.maximized = Some(false);
        root.minimized = Some(false);
    }
    h.step();
    assert!(calls_since(&h, before, "window_placement_set").is_empty(), "not on the first frame");
    for _ in 0..60 {
        h.step();
    }
    let saved = calls_since(&h, before, "window_placement_set");
    assert_eq!(saved.len(), 1, "{saved:?}");
    assert_eq!(saved[0]["window"], "main");
    assert_eq!((saved[0]["left"].as_i64(), saved[0]["width"].as_i64()), (Some(40), Some(1296)));
}

#[test]
fn quitting_saves_bounds_that_have_not_settled_yet() {
    let mut h = h();
    let before = h.state().submitted().len();
    {
        let root = h.input_mut().viewports.entry(egui::ViewportId::ROOT).or_default();
        root.outer_rect = Some(egui::Rect::from_min_size(egui::pos2(70.0, 60.0), vec2(1296.0, 839.0)));
        root.inner_rect = Some(egui::Rect::from_min_size(egui::pos2(78.0, 91.0), vec2(1280.0, 800.0)));
        root.maximized = Some(false);
        root.minimized = Some(false);
    }
    h.step();
    assert!(calls_since(&h, before, "window_placement_set").is_empty(), "still settling");
    // Quit by the title bar / menu / `app_quit` (not the window's close): saved at once.
    let ctx = h.ctx.clone();
    h.state_mut().run(&ctx, Action::AskQuit);
    let saved = calls_since(&h, before, "window_placement_set");
    assert_eq!(saved.len(), 1, "{saved:?}");
    assert_eq!((saved[0]["window"].as_str(), saved[0]["left"].as_i64()), (Some("main"), Some(70)));
}

fn calls_since(h: &Harness<'static, csm_ui::CsmApp>, before: usize, id: &str) -> Vec<serde_json::Value> {
    h.state().submitted()[before..].iter().filter(|c| c.id == id).map(|c| c.params.clone()).collect()
}

fn order(h: &Harness<'static, csm_ui::CsmApp>) -> Vec<String> {
    h.state().model.sessions.iter().map(|s| s.id.clone()).collect()
}

#[test]
fn edit_session_fills_the_new_session_panel_and_saves_with_session_edit() {
    let mut h = h();
    // The row's context menu → Edit session…
    let ctx = h.ctx.clone();
    h.state_mut().run(&ctx, Action::OpenMenu(csm_ui::app::Menu::Row { session: "web".into(), at: egui::pos2(300.0, 330.0) }));
    for _ in 0..3 {
        h.step();
    }
    h.get_by_label("Edit session…").click();
    settle(&mut h);
    {
        let q = &h.state().ui.quick;
        assert_eq!(q.edit.as_deref(), Some("web"));
        assert_eq!(q.name, "Odulodo.Web", "filled in from the session");
        assert_eq!(q.query, "C:\\GitHub\\Odulodo.Web");
    }
    assert!(has(&h, "Session name"));
    h.state_mut().ui.quick.name = "Web (brand facet)".into();
    // Edit: Tab goes from the folder to the name, Browse, then the command row.
    for _ in 0..3 {
        h.key_press(egui::Key::Tab);
        h.step();
    }
    h.key_press(egui::Key::ArrowRight); // claude → pwsh
    h.step();
    h.key_press(egui::Key::Enter);
    settle(&mut h);
    let (id, params) = last_call(&h);
    assert_eq!(id, "session_edit");
    assert_eq!(params["session_id"], "web");
    assert_eq!(params["name"], "Web (brand facet)");
    assert_eq!(params["command"], "pwsh");
    assert_eq!(params["kind"], "local");
    assert_eq!(params["folder"], "C:\\GitHub\\Odulodo.Web");
    assert_ne!(h.state().ui.overlay, csm_ui::app::Overlay::QuickOpen, "the panel closes on save");
    assert_eq!(h.state().model.session("web").unwrap().name, "Web (brand facet)");
}

#[test]
fn dragging_a_row_onto_a_rail_tab_moves_it_to_that_group() {
    let mut h = h();
    let before = h.state().submitted().len();
    let from = h.get_by_label("Session: Odulodo.Web (PS)").rect().center();
    let to = h.get_by_label("Group tab: Umage").rect().center();
    drag(&mut h, from, to - from);
    let moves = calls_since(&h, before, "session_move_to_group");
    assert_eq!(moves, vec![serde_json::json!({"session_id": "webps", "group_id": "umage"})]);
    assert_eq!(h.state().model.session("webps").unwrap().group.as_deref(), Some("umage"));
    assert!(!has(&h, "Session: Odulodo.Web (PS)"), "it left the Odulodo tab");
    assert!(h.state().ui.drags.row.is_none(), "the drag ended");
}

#[test]
fn dragging_a_row_between_rows_reorders_by_anchor() {
    let mut h = h();
    let before = h.state().submitted().len();
    // Odulodo (first) to just below Odulodo.Web (PS) (the tab's last live row).
    let from = h.get_by_label("Session: Odulodo").rect().center();
    let last = h.get_by_label("Session: Odulodo.Web (PS)").rect();
    let to = egui::pos2(last.center().x, last.max.y - 3.0);
    drag(&mut h, from, to - from);
    let moves = calls_since(&h, before, "session_move_to_group");
    assert_eq!(moves.len(), 1, "{moves:?}");
    assert_eq!(moves[0]["session_id"], "odulodo");
    assert_eq!(moves[0]["after"], "webps", "an anchor, not an index");
    assert_eq!(moves[0]["group_id"], "odulodo", "same group");
    let o = order(&h);
    let pos = |id: &str| o.iter().position(|x| x == id).unwrap();
    assert_eq!(pos("odulodo"), pos("webps") + 1);
}

#[test]
fn dropping_a_row_on_a_group_header_moves_it_there() {
    let mut h = h();
    h.get_by_label("Group view: layers").click();
    settle(&mut h);
    let before = h.state().submitted().len();
    let from = h.get_by_label("Session: Odulodo.Web (PS)").rect().center();
    let to = h.get_by_label("Group: Ops").rect().center();
    drag(&mut h, from, to - from);
    let moves = calls_since(&h, before, "session_move_to_group");
    assert_eq!(moves.len(), 1, "{moves:?}");
    assert_eq!(moves[0]["group_id"], "ops");
    assert_eq!(h.state().model.session("webps").unwrap().group.as_deref(), Some("ops"));
}

#[test]
fn free_mode_a_header_dropped_on_a_panes_edge_splits_it() {
    let mut h = h();
    h.get_by_label("Layout: Free").click();
    settle(&mut h);
    let before = h.state().submitted().len();
    // Odulodo's header (its name, left of its chips) onto the right edge of Odulodo.Web (PS).
    let head = h.get_by_label("Pane: Odulodo").rect();
    let from = egui::pos2(head.min.x + 30.0, head.center().y);
    let target = h.get_by_label("Pane: Odulodo.Web (PS)").rect();
    let to = egui::pos2(target.max.x - 12.0, target.max.y + 80.0);
    drag(&mut h, from, to - from);
    let sets = calls_since(&h, before, "layout_set");
    assert_eq!(sets.len(), 1, "{sets:?}");
    let tree = layout(&h).tree.unwrap();
    // Odulodo now sits right of Odulodo.Web (PS), in a split of their own.
    let pair = csm_ui::model::SplitNode::Split {
        dir: csm_ui::model::SplitDir::Row,
        ratios: vec![0.5, 0.5],
        children: vec![csm_ui::model::SplitNode::leaf("webps"), csm_ui::model::SplitNode::leaf("odulodo")],
    };
    fn has_node(n: &csm_ui::model::SplitNode, want: &csm_ui::model::SplitNode) -> bool {
        n == want || matches!(n, csm_ui::model::SplitNode::Split { children, .. } if children.iter().any(|c| has_node(c, want)))
    }
    assert!(has_node(&tree, &pair), "{tree:?}");
}

#[test]
fn f2_renames_the_active_session_in_place() {
    let mut h = h();
    assert_eq!(h.state().model.active.as_deref(), Some("web"));
    h.key_press(egui::Key::F2);
    settle(&mut h);
    assert!(has(&h, "Rename session"), "a field in the row, not a dialog");
    assert_eq!(h.state().ui.overlay, csm_ui::app::Overlay::None);
    h.state_mut().ui.inline.as_mut().unwrap().text = "Brand facet".into();
    h.key_press(egui::Key::Enter);
    settle(&mut h);
    assert_eq!(last_call(&h), ("session_rename".to_string(), serde_json::json!({"session_id": "web", "name": "Brand facet"})));
    assert!(has(&h, "Session: Brand facet"));
    // Esc cancels.
    h.key_press(egui::Key::F2);
    settle(&mut h);
    let n = h.state().submitted().len();
    h.state_mut().ui.inline.as_mut().unwrap().text = "nope".into();
    h.key_press(egui::Key::Escape);
    settle(&mut h);
    assert_eq!(h.state().submitted().len(), n, "nothing saved");
    assert!(h.state().ui.inline.is_none());
}

#[test]
fn new_group_and_group_rename_happen_in_place() {
    let mut h = h();
    h.get_all_by_label("New group").next().unwrap().click();
    settle(&mut h);
    assert!(has(&h, "New group name"));
    h.state_mut().ui.inline.as_mut().unwrap().text = "Client X".into();
    h.key_press(egui::Key::Enter);
    settle(&mut h);
    assert_eq!(last_call(&h), ("group_new".to_string(), serde_json::json!({"name": "Client X"})));
    assert!(has(&h, "Group tab: Client X"));
    let ctx = h.ctx.clone();
    let gid = h.state().model.groups.iter().find(|g| g.name == "Client X").unwrap().id.clone();
    h.state_mut().run(&ctx, Action::StartGroupRename(gid.clone()));
    settle(&mut h);
    assert!(has(&h, "Rename group"));
    h.state_mut().ui.inline.as_mut().unwrap().text = "Client Y".into();
    h.key_press(egui::Key::Enter);
    settle(&mut h);
    assert_eq!(last_call(&h), ("group_rename".to_string(), serde_json::json!({"group_id": gid, "name": "Client Y"})));
}

#[test]
fn ctrl_tab_cycles_sessions_and_ctrl_digits_pick_rail_tabs() {
    let mut h = h();
    assert_eq!(h.state().model.active.as_deref(), Some("web"));
    h.key_press_modifiers(Modifiers::CTRL, egui::Key::Tab);
    settle(&mut h);
    assert_eq!(h.state().model.active.as_deref(), Some("wt"), "the next live session of the tab");
    h.key_press_modifiers(Modifiers::CTRL | Modifiers::SHIFT, egui::Key::Tab);
    settle(&mut h);
    assert_eq!(h.state().model.active.as_deref(), Some("web"));
    h.key_press_modifiers(Modifiers::COMMAND, egui::Key::Num3);
    settle(&mut h);
    assert_eq!(h.state().model.rail, Scope::Group("umage".into()), "Ctrl+3: the second group (1 is All)");
    h.key_press_modifiers(Modifiers::COMMAND, egui::Key::Num1);
    settle(&mut h);
    assert_eq!(h.state().model.rail, Scope::All);
}

#[test]
fn the_help_overlay_lists_the_shortcuts() {
    let mut h = h();
    h.key_press(egui::Key::F1);
    settle(&mut h);
    assert!(has(&h, "Shortcut: Ctrl+K: Command palette"));
    assert!(has(&h, "Shortcut: F2: Rename the active session"));
    h.key_press(egui::Key::Escape);
    settle(&mut h);
    assert!(!has(&h, "Shortcut: Ctrl+K: Command palette"));
    // The title bar's ? button too.
    h.get_by_label_contains("Keyboard shortcuts").click();
    settle(&mut h);
    assert!(has(&h, "Shortcut: Ctrl+T: New session"));
}

#[test]
fn reset_accent_colour_is_offered_only_for_a_pushed_colour() {
    let mut h = h();
    h.get_by_label("Group tab: Ops").click();
    settle(&mut h);
    let ctx = h.ctx.clone();
    h.state_mut().run(&ctx, Action::OpenMenu(csm_ui::app::Menu::Row { session: "gw".into(), at: egui::pos2(300.0, 330.0) }));
    for _ in 0..3 {
        h.step();
    }
    assert!(!has(&h, "Reset accent colour"), "api-gateway's colour comes from its folder");
    h.state_mut().run(&ctx, Action::CloseMenu);
    h.state_mut().run(&ctx, Action::OpenMenu(csm_ui::app::Menu::Row { session: "build".into(), at: egui::pos2(300.0, 330.0) }));
    for _ in 0..3 {
        h.step();
    }
    h.get_by_label("Reset accent colour").click();
    settle(&mut h);
    assert_eq!(last_call(&h), ("session_accent_reset".to_string(), serde_json::json!({"session_id": "build"})));
    assert!(!h.state().model.session("build").unwrap().accent_overridden);
}

#[test]
fn busy_close_dont_warn_again_turns_the_warning_off() {
    let mut h = h();
    let ctx = h.ctx.clone();
    h.state_mut().run(&ctx, Action::AskQuit);
    settle(&mut h);
    assert!(has(&h, "Keep running"));
    h.get_by_label("Don't warn me again").click();
    h.step();
    h.get_by_label("Close anyway").click();
    settle(&mut h);
    let calls: Vec<String> = h.state().submitted().iter().map(|c| c.id.clone()).collect();
    let n = calls.len();
    assert_eq!(&calls[n - 2..], ["settings_set", "app_quit"]);
    assert_eq!(h.state().submitted()[n - 2].params, serde_json::json!({"key": "WarnOnCloseWhenBusy", "value": false}));
    assert!(!h.state().prefs.warn_on_close_when_busy, "the app follows the engine's answer");
}

#[test]
fn settings_round_trip_through_the_engine() {
    let mut h = h();
    h.get_by_label_contains("Settings  Ctrl+,").click();
    settle(&mut h);
    assert!(h.state().ui.settings.open);
    assert!(has(&h, "Restore sessions on start"));
    let engine = h.state().dispatch.engine().expect("an engine port");
    let get = |k: &str| engine.call(csm_engine::Caller::Ui, "settings_get", serde_json::json!({"key": k})).unwrap().data["value"].clone();
    assert_eq!(get("AutoRestoreSessions"), true);
    h.get_by_label("Restore sessions on start").click();
    settle(&mut h);
    assert_eq!(get("AutoRestoreSessions"), false, "the engine has it");
    assert_eq!(h.state().ui.settings.bool("AutoRestoreSessions"), Some(false), "the page shows the engine's answer");
    // Appearance: the theme follows.
    h.get_by_label("Settings: Appearance").click();
    settle(&mut h);
    h.get_by_label("Theme: Light").click();
    settle(&mut h);
    assert_eq!(get("Theme"), "light");
    assert_eq!(h.state().model.theme, csm_ui::theme::ThemeKind::Latte);
    // Terminal padding is live.
    let ctx = h.ctx.clone();
    h.state_mut().run(&ctx, Action::Tool("settings_set", serde_json::json!({"key": "TerminalPadding", "value": 16})));
    settle(&mut h);
    assert_eq!(h.state().prefs.terminal_padding, 16.0);
    assert_eq!(csm_ui::terminal::lock(&h.state().host).padding, 16.0);
    // A launch command added in General reaches the engine.
    h.get_by_label("Settings: General").click();
    settle(&mut h);
    h.state_mut().ui.settings.new_command = "claude --model haiku".into();
    h.get_by_label("Add").click();
    settle(&mut h);
    let cmds = get("LaunchCommands");
    assert!(cmds.as_array().unwrap().iter().any(|c| c == "claude --model haiku"), "{cmds}");
    h.key_press(egui::Key::Escape);
    settle(&mut h);
    assert!(!h.state().ui.settings.open, "Esc closes the page");
}

#[test]
fn an_idle_window_does_not_repaint() {
    // Nothing working, starting or restoring: after the first frames the window asks for no
    // repaint at all (spinners are the only timers, and only while one is on screen).
    let mut b = backend(ThemeKind::Mocha);
    for s in &mut b.model_mut().sessions {
        if matches!(s.status, csm_ui::model::Status::Working | csm_ui::model::Status::Starting) {
            s.status = csm_ui::model::Status::Idle;
            s.progress = None;
        }
    }
    b.publish();
    let mut h = harness(app_with(b));
    settle(&mut h);
    for _ in 0..5 {
        h.step();
    }
    let delay = |h: &Harness<'static, csm_ui::CsmApp>| h.output().viewport_output.get(&egui::ViewportId::ROOT).map(|v| v.repaint_delay).unwrap();
    assert_eq!(delay(&h), std::time::Duration::MAX, "an idle window schedules no repaint: {:?}", h.ctx.repaint_causes());

    // A working session on screen animates at the spinner's rate, not every frame.
    let mut h = harness(app_with(backend(ThemeKind::Mocha)));
    settle(&mut h);
    h.step();
    let d = delay(&h);
    assert!(d >= std::time::Duration::from_millis(50) && d < std::time::Duration::from_secs(1), "spinner cadence: {d:?}");
}

#[test]
fn the_wsl_fields_ask_for_the_installed_distributions_once() {
    let mut h = h();
    let ctx = h.ctx.clone();
    h.state_mut().run(&ctx, Action::OpenQuickOpen { group: None });
    settle(&mut h);
    h.state_mut().ui.quick.remote = true;
    h.state_mut().ui.quick.rkind = csm_ui::app::RemoteKind::Wsl;
    for _ in 0..3 {
        settle(&mut h);
    }
    let asked = h.state().submitted().iter().filter(|c| c.id == "wsl_distros").count();
    assert_eq!(asked, 1, "asked once, not every frame");
    assert!(h.state().ui.quick.distros.is_some());
}

#[test]
fn the_window_icon_is_the_logo() {
    let icon = csm_ui::app::app_icon(64);
    assert_eq!((icon.width, icon.height, icon.rgba.len()), (64, 64, 64 * 64 * 4));
    assert_eq!(icon.rgba[3], 0, "rounded corner: transparent");
    assert_eq!(icon.rgba[(32 * 64 + 4) * 4 + 3], 255, "the square is opaque");
}

/// Clicks a control that may sit below the fold of a scrolling page.
fn click_far(h: &mut Harness<'static, csm_ui::CsmApp>, label: &str) {
    h.get_by_label(label).scroll_to_me();
    for _ in 0..3 {
        h.step();
    }
    h.get_by_label(label).click();
    settle(h);
}

#[test]
fn notification_toggles_decide_what_reaches_the_os() {
    use csm_ui::model::Status;
    let mut h = h();
    let ctx = h.ctx.clone();
    let engine = h.state().dispatch.engine().expect("an engine port");
    let get = |k: &str| engine.call(csm_engine::Caller::Ui, "settings_get", serde_json::json!({"key": k})).unwrap().data["value"].clone();
    // Quiet sessions that are not the one in front.
    let quiet: Vec<String> = {
        let m = &h.state().model;
        m.sessions
            .iter()
            .filter(|s| matches!(s.status, Status::Idle | Status::Working) && Some(&s.id) != m.active.as_ref())
            .map(|s| s.id.clone())
            .take(3)
            .collect()
    };
    assert!(quiet.len() >= 3, "the demo has quiet sessions");
    let set_status = |h: &mut Harness<'static, csm_ui::CsmApp>, id: &str, st: &str| {
        h.state_mut().run(&ctx, Action::Tool("demo_set_status", serde_json::json!({"session_id": id, "status": st})));
        settle(h);
    };
    // Off by default (as in WPF): a session that starts needing you sends nothing.
    assert_eq!(get("ShowToastNotifications"), false);
    set_status(&mut h, &quiet[0], "approval");
    assert!(h.state_mut().take_notifications().is_empty(), "notifications are off by default");

    // Turned on in Settings → General; the engine has it and the app follows.
    h.get_by_label_contains("Settings  Ctrl+,").click();
    settle(&mut h);
    click_far(&mut h, "Show system notifications");
    settle(&mut h);
    assert_eq!(get("ShowToastNotifications"), true);
    assert!(h.state().prefs.notifications);
    set_status(&mut h, &quiet[1], "question");
    let sent = h.state_mut().take_notifications();
    assert_eq!(sent.len(), 1, "{sent:?}");
    assert_eq!(sent[0].session_id, quiet[1]);
    assert!(!sent[0].sound, "no sound unless asked for");
    assert!(sent[0].title.starts_with("CodeShellManager — "));
    // Staying there is not news.
    for _ in 0..5 {
        h.step();
    }
    assert!(h.state_mut().take_notifications().is_empty());

    // The sound follows its own toggle.
    click_far(&mut h, "Play a sound with notifications");
    settle(&mut h);
    assert_eq!(get("ShowNotificationSound"), true);
    set_status(&mut h, &quiet[2], "error");
    let sent = h.state_mut().take_notifications();
    assert_eq!(sent.len(), 1, "{sent:?}");
    assert!(sent[0].sound);

    // The test button sends one whatever the state.
    click_far(&mut h, "Send a test notification");
    settle(&mut h);
    assert_eq!(last_call(&h).0, "notification_test");
    let sent = h.state_mut().take_notifications();
    assert_eq!(sent.len(), 1, "{sent:?}");
    assert!(sent[0].session_id.is_empty());

    // Off again: nothing.
    click_far(&mut h, "Show system notifications");
    settle(&mut h);
    assert!(!h.state().prefs.notifications);
    set_status(&mut h, &quiet[0], "idle");
    set_status(&mut h, &quiet[0], "error");
    assert!(h.state_mut().take_notifications().is_empty());
    let needs_you = h.state().model.sessions.iter().filter(|s| s.status.needs_you()).count();
    assert!(needs_you >= 1);
    assert_eq!(h.state().needs_you_count(), needs_you);
}

#[test]
fn window_show_restores_and_focuses_the_main_window() {
    let mut h = h();
    let ctx = h.ctx.clone();
    h.state_mut().run(&ctx, Action::Tool("window_show", serde_json::json!({})));
    assert_eq!(last_call(&h).0, "window_show");
    let mut cmds = Vec::new();
    for _ in 0..10 {
        h.step();
        if let Some(v) = h.output().viewport_output.get(&egui::ViewportId::ROOT) {
            cmds.extend(v.commands.iter().cloned());
        }
    }
    for want in [egui::ViewportCommand::Visible(true), egui::ViewportCommand::Minimized(false), egui::ViewportCommand::Focus] {
        assert!(cmds.contains(&want), "{want:?} in {cmds:?}");
    }
}

#[test]
fn about_promotes_umage_ai_and_its_links_open_through_the_open_tool() {
    let mut h = h();
    let ctx = h.ctx.clone();
    h.state_mut().run(&ctx, Action::OpenSettings);
    h.state_mut().ui.settings.section = csm_ui::app::SettingsSection::About;
    settle(&mut h);
    assert!(h.query_by_label("Made by ").is_some(), "the umage.ai byline");
    assert!(h.query_by_label("umage.ai logo").is_some(), "the official logo, not a typed wordmark");
    assert!(h.query_by_label("Agentic development").is_some());
    assert!(h.query_by_label("Diagnostics").is_some(), "the diagnostics stay");
    for (label, _, url) in csm_ui::screens::settings::ABOUT_LINKS {
        h.get_by_label(label).click();
        h.step();
        let c = h.state().submitted().last().cloned().expect("a call was submitted");
        assert_eq!((c.id.as_str(), c.params["url"].as_str()), ("open_url", Some(url)), "{label}");
    }
}

#[test]
fn side_by_side_keeps_the_unpinned_pane_when_a_pin_is_clicked() {
    let mut h = h();
    // The demo's Odulodo group pins three sessions; search-sort is not pinned.
    assert_eq!(layout(&h).mode, LayoutMode::Side);
    h.get_by_label("Session: search-sort").click();
    settle(&mut h);
    assert!(has(&h, "Pane: search-sort"), "the active unpinned session joins the pins");
    h.get_by_label("Session: Odulodo").click();
    settle(&mut h);
    assert_eq!(h.state().model.active.as_deref(), Some("odulodo"));
    assert!(has(&h, "Pane: search-sort"), "clicking a pinned session keeps the unpinned one beside it");
    assert!(has(&h, "Pane: Odulodo.Web"), "the pins stay");
}

#[test]
fn the_pane_menu_button_is_labelled_when_there_is_room() {
    let mut h = h();
    h.get_by_label("Layout: Focus").click();
    settle(&mut h);
    // One wide pane: "Actions ▾", wider than an icon button; it opens the pane menu.
    let b = h.get_by_label("Session actions  (Ctrl+.)");
    assert!(b.rect().width() > csm_ui::theme::size::CONTROL_SM * 2.0, "labelled, not just ⋯");
    b.click();
    settle(&mut h);
    assert!(matches!(h.state().ui.menu, Some(csm_ui::app::Menu::Pane { .. })), "the pane menu opened");
    // Overview's mini panes have no menu button; a narrow Side by side pane falls back to ⋯.
}

#[test]
fn the_inspector_edge_drags_its_width_and_a_double_click_resets_it() {
    let mut h = h();
    h.get_by_label_contains("Inspector  Ctrl+I").click();
    settle(&mut h);
    let edge = h.get_by_label("Inspector edge").rect().center();
    drag(&mut h, edge, egui::vec2(-120.0, 0.0));
    let (id, params) = last_call(&h);
    assert_eq!(id, "inspector_panel_set");
    let w = params["width"].as_f64().expect("a width");
    assert!(w > f64::from(csm_ui::theme::size::INSPECTOR) + 60.0, "dragging left widens it ({w})");
    assert_eq!(h.state().model.inspector_width.map(f64::from), Some(w));
    // The edge followed the new width; double-clicking it goes back to the default.
    let edge = h.get_by_label("Inspector edge").rect().center();
    h.event(egui::Event::PointerMoved(edge));
    h.step();
    // Two clicks, a frame apart: inside the double-click window.
    for _ in 0..2 {
        h.event(egui::Event::PointerButton { pos: edge, button: egui::PointerButton::Primary, pressed: true, modifiers: egui::Modifiers::NONE });
        h.step();
        h.event(egui::Event::PointerButton { pos: edge, button: egui::PointerButton::Primary, pressed: false, modifiers: egui::Modifiers::NONE });
        h.step();
    }
    settle(&mut h);
    settle(&mut h);
    assert_eq!(last_call(&h), ("inspector_panel_set".to_string(), serde_json::json!({"reset": true})));
    assert_eq!(h.state().model.inspector_width, None);
}
