//! The pane window bar (minimize, maximize/restore, close) on every pane, and resizing floating
//! panes from their edges and corners: through the real UI (egui_kittest + AccessKit) on the demo
//! backend and a real engine, so every click is a tool call.
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

mod common;

use common::*;
use csm_ui::Action;
use csm_ui::model::{Layout, LayoutMode, Status};
use csm_ui::theme::{ThemeKind, size};
use egui::{Event, Modifiers, PointerButton, Pos2, Rect, vec2};
use egui_kittest::Harness;
use egui_kittest::kittest::Queryable;
use serde_json::json;

type H = Harness<'static, csm_ui::CsmApp>;

fn h() -> H {
    let mut h = harness(app_with(backend(ThemeKind::Mocha)));
    settle(&mut h);
    h
}

fn has(h: &H, label: &str) -> bool {
    h.query_by_label(label).is_some()
}

fn layout(h: &H) -> Layout {
    let m = &h.state().model;
    m.layout(&m.pane_scope())
}

fn scope_key(h: &H) -> String {
    h.state().model.pane_scope().key()
}

fn calls_since(h: &H, before: usize, id: &str) -> Vec<serde_json::Value> {
    h.state().submitted()[before..].iter().filter(|c| c.id == id).map(|c| c.params.clone()).collect()
}

fn panes(h: &H) -> usize {
    h.query_all_by_label_contains("Pane: ").count()
}

fn header(h: &H, name: &str) -> Rect {
    h.get_by_label(&format!("Pane: {name}")).rect()
}

/// The control labelled `label` in the header of the pane named `name`.
fn control(h: &H, name: &str, label: &str) -> Pos2 {
    let head = header(h, name).expand2(vec2(size::SPLIT_GRIP, 0.0));
    h.query_all_by_label(label)
        .map(|n| n.rect())
        .find(|r| head.contains(r.center()))
        .unwrap_or_else(|| panic!("no {label} in the header of {name}"))
        .center()
}

fn click_at(h: &mut H, p: Pos2) {
    h.event(Event::PointerMoved(p));
    h.step();
    h.event(Event::PointerButton { pos: p, button: PointerButton::Primary, pressed: true, modifiers: Modifiers::NONE });
    h.step();
    h.event(Event::PointerButton { pos: p, button: PointerButton::Primary, pressed: false, modifiers: Modifiers::NONE });
    h.step();
    settle(h);
}

fn double_click_at(h: &mut H, p: Pos2) {
    h.event(Event::PointerMoved(p));
    // A second of quiet first, so egui doesn't count it as a triple click with earlier ones.
    for _ in 0..60 {
        h.step();
    }
    for _ in 0..2 {
        h.event(Event::PointerButton { pos: p, button: PointerButton::Primary, pressed: true, modifiers: Modifiers::NONE });
        h.step();
        h.event(Event::PointerButton { pos: p, button: PointerButton::Primary, pressed: false, modifiers: Modifiers::NONE });
        h.step();
    }
    settle(h);
}

fn press(h: &mut H, from: Pos2) {
    h.event(Event::PointerMoved(from));
    h.step();
    h.event(Event::PointerButton { pos: from, button: PointerButton::Primary, pressed: true, modifiers: Modifiers::NONE });
    h.step();
}

fn move_to(h: &mut H, from: Pos2, by: egui::Vec2) {
    for i in 1..=6 {
        h.event(Event::PointerMoved(from + by * (i as f32 / 6.0)));
        h.step();
    }
}

fn release(h: &mut H, at: Pos2) {
    h.event(Event::PointerButton { pos: at, button: PointerButton::Primary, pressed: false, modifiers: Modifiers::NONE });
    h.step();
    settle(h);
}

fn drag(h: &mut H, from: Pos2, by: egui::Vec2) {
    press(h, from);
    move_to(h, from, by);
    release(h, from + by);
}

fn float_rect(h: &H, id: &str) -> [f32; 4] {
    layout(h).floats.iter().find(|f| f.session == id).map(|f| f.rect).expect("floating")
}

/// The columns the pane's terminal view measured last frame.
fn cols(h: &H, id: &str) -> u16 {
    let host = h.state().host.lock().unwrap();
    host.views.get(&format!("main:{id}")).and_then(|v| v.grid()).map(|g| g.cols).expect("a measured terminal")
}

/// The header's hit rect (its AccessKit rect) is the header less `space::SM` each side.
fn theme_space_sm() -> f32 {
    csm_ui::theme::space::SM
}

fn near(a: f32, b: f32) -> bool {
    (a - b).abs() < 1.5
}

/// Free mode with Odulodo.Web (PS) floating over the split tree.
fn floating() -> H {
    let mut h = h();
    h.get_by_label("Layout: Free").click();
    settle(&mut h);
    let ctx = h.ctx.clone();
    let key = scope_key(&h);
    // Only this one floats (the demo's Free layout has another), so its handles are unique.
    for f in layout(&h).floats {
        h.state_mut().run(&ctx, Action::Tool("pane_dock", json!({"session_id": f.session, "group": key})));
    }
    h.state_mut().run(&ctx, Action::Tool("pane_float", json!({"session_id": "webps", "group": key})));
    settle(&mut h);
    assert_eq!(layout(&h).floats.len(), 1);
    h
}

#[test]
fn a_floating_pane_resizes_from_its_edges_live_and_saves_on_release() {
    let mut h = floating();
    let start = float_rect(&h, "webps");
    let start_cols = cols(&h, "webps");
    for (_, label) in csm_ui::screens::panes::Grip::ALL {
        assert!(has(&h, label), "{label} handle");
    }

    // The right edge, 60 points out: live while dragging (nothing saved yet), one layout_set on release.
    let before = h.state().submitted().len();
    let edge = h.get_by_label("Resize right edge").rect().center();
    press(&mut h, edge);
    move_to(&mut h, edge, vec2(60.0, 0.0));
    let live = h.state().ui.drags.float.clone().expect("a live rect while dragging");
    assert!(near(live.1[2], start[2] + 60.0), "live width {:?}", live.1);
    h.step(); // the rect a frame's drag computes is drawn by the next frame
    let head_w = h.get_by_label("Pane: Odulodo.Web (PS)").rect().width();
    assert!(near(head_w, start[2] + 60.0 - 2.0 * theme_space_sm()), "the pane follows the pointer while dragging ({head_w})");
    assert!(calls_since(&h, before, "layout_set").is_empty(), "nothing saved mid-drag");
    // The terminal inside re-measures while dragging (that grid is what the PTY is resized to).
    let live_cols = cols(&h, "webps");
    assert!(live_cols > start_cols, "{start_cols} → {live_cols} columns while dragging");
    release(&mut h, edge + vec2(60.0, 0.0));
    assert_eq!(cols(&h, "webps"), live_cols, "and keeps that size once saved");
    let sets = calls_since(&h, before, "layout_set");
    assert_eq!(sets.len(), 1, "{sets:?}");
    assert_eq!(sets[0]["free"]["floats"][0]["session"], "webps");
    let r = float_rect(&h, "webps");
    assert!(near(r[0], start[0]) && near(r[1], start[1]) && near(r[3], start[3]), "{r:?}");
    assert!(near(r[2], start[2] + 60.0), "{r:?}");
    assert!(h.state().ui.drags.float.is_none(), "the drag ended");

    // The left edge far to the right: the minimum width holds, the right edge stays put.
    let edge = h.get_by_label("Resize left edge").rect().center();
    drag(&mut h, edge, vec2(400.0, 0.0));
    let r2 = float_rect(&h, "webps");
    assert!(near(r2[2], size::FLOAT_MIN[0]), "min width {r2:?}");
    assert!(near(r2[0] + r2[2], r[0] + r[2]), "the right edge stays: {r2:?}");

    // The top edge up past the canvas: clamped at its top.
    let edge = h.get_by_label("Resize top edge").rect().center();
    drag(&mut h, edge, vec2(0.0, -(r2[1] + 200.0)));
    let r3 = float_rect(&h, "webps");
    assert!(near(r3[1], 0.0), "clamped to the canvas: {r3:?}");
    assert!(near(r3[1] + r3[3], r2[1] + r2[3]), "the bottom edge stays: {r3:?}");
}

#[test]
fn the_corner_grip_resizes_both_ways_and_the_header_still_moves_it() {
    let mut h = floating();
    let start = float_rect(&h, "webps");
    let grip = h.get_by_label("Drag to resize").rect().center();
    drag(&mut h, grip, vec2(-40.0, 50.0));
    let r = float_rect(&h, "webps");
    assert!(near(r[2], start[2] - 40.0) && near(r[3], start[3] + 50.0), "{r:?}");
    assert!(near(r[0], start[0]) && near(r[1], start[1]), "{r:?}");
    // A corner past the minimum: both minimums hold.
    let grip = h.get_by_label("Resize top-left corner").rect().center();
    drag(&mut h, grip, vec2(600.0, 600.0));
    let r2 = float_rect(&h, "webps");
    assert!(near(r2[2], size::FLOAT_MIN[0]) && near(r2[3], size::FLOAT_MIN[1]), "{r2:?}");

    // Move by the header (left of its controls).
    let head = header(&h, "Odulodo.Web (PS)");
    let from = egui::pos2(head.center().x - 20.0, head.center().y);
    let before = h.state().submitted().len();
    drag(&mut h, from, vec2(-30.0, 20.0));
    let r3 = float_rect(&h, "webps");
    assert!(near(r3[0], r2[0] - 30.0) && near(r3[1], r2[1] + 20.0), "{r3:?}");
    assert!(near(r3[2], r2[2]) && near(r3[3], r2[3]), "a move keeps the size: {r3:?}");
    assert_eq!(calls_since(&h, before, "layout_set").len(), 1);
}

#[test]
fn maximize_and_restore_round_trip() {
    let mut h = h();
    let before_layout = layout(&h);
    let n = panes(&h);
    assert!(n >= 2, "the demo shows several panes ({n})");
    let p = control(&h, "Odulodo", "Maximize");
    click_at(&mut h, p);
    assert_eq!(h.state().submitted().last().unwrap().id, "pane_maximize");
    assert_eq!(layout(&h).maximized.as_deref(), Some("odulodo"));
    assert_eq!(panes(&h), 1, "it fills the canvas; the others are hidden");
    assert!(header(&h, "Odulodo").width() > 900.0, "the whole canvas wide");
    assert!(h.state().model.session("web").unwrap().status.is_live(), "the others keep running");
    // The same button is Restore now.
    let p = control(&h, "Odulodo", "Restore");
    click_at(&mut h, p);
    assert_eq!(h.state().submitted().last().unwrap().id, "pane_restore");
    assert_eq!(layout(&h), before_layout, "back to the layout exactly");
    assert_eq!(panes(&h), n);

    // Double-clicking the header toggles too.
    let head = header(&h, "Odulodo");
    let at = egui::pos2(head.min.x + 30.0, head.center().y);
    click_at(&mut h, at); // activate it first: the double-click then only toggles
    let before = h.state().submitted().len();
    double_click_at(&mut h, at);
    let ids: Vec<String> = h.state().submitted()[before..].iter().map(|c| format!("{} {}", c.id, c.params)).collect();
    assert_eq!(layout(&h).maximized.as_deref(), Some("odulodo"), "{ids:?} active {:?}", h.state().model.active);
    let head = header(&h, "Odulodo");
    double_click_at(&mut h, egui::pos2(head.min.x + 30.0, head.center().y));
    assert_eq!(layout(&h), before_layout);
}

#[test]
fn a_floating_pane_maximizes_to_the_canvas() {
    let mut h = floating();
    let p = control(&h, "Odulodo.Web (PS)", "Maximize");
    click_at(&mut h, p);
    assert_eq!(layout(&h).maximized.as_deref(), Some("webps"));
    assert_eq!(panes(&h), 1);
    assert!(header(&h, "Odulodo.Web (PS)").width() > 900.0);
    assert!(!has(&h, "Drag to resize"), "no resize handles while maximized");
    let p = control(&h, "Odulodo.Web (PS)", "Restore");
    click_at(&mut h, p);
    assert_eq!(layout(&h).maximized, None);
    assert_eq!(layout(&h).floats.len(), 1, "still floating");
    assert!(has(&h, "Drag to resize"));
}

#[test]
fn minimize_then_restore_from_its_chip_and_from_the_sidebar() {
    let mut h = h();
    let n = panes(&h);
    let p = control(&h, "Odulodo.Web", "Minimize");
    click_at(&mut h, p);
    let c = h.state().submitted().last().cloned().unwrap();
    assert_eq!((c.id.as_str(), c.params["session_id"].as_str()), ("pane_minimize", Some("web")));
    assert_eq!(layout(&h).minimized, vec!["web".to_string()]);
    assert!(!has(&h, "Pane: Odulodo.Web"), "off the canvas");
    assert_eq!(panes(&h), n - 1);
    assert!(has(&h, "Minimized: Odulodo.Web"), "a chip in the strip");
    assert!(h.state().model.session("web").unwrap().status.is_live(), "still running");

    h.get_by_label("Minimized: Odulodo.Web").click();
    settle(&mut h);
    assert_eq!(h.state().submitted().last().unwrap().id, "pane_restore");
    assert!(layout(&h).minimized.is_empty());
    assert!(has(&h, "Pane: Odulodo.Web"));
    assert!(!has(&h, "Minimized: Odulodo.Web"));

    // Minimized again, then focused from the sidebar: it comes back.
    let p = control(&h, "Odulodo.Web", "Minimize");
    click_at(&mut h, p);
    assert!(!has(&h, "Pane: Odulodo.Web"));
    h.get_by_label("Session: Odulodo.Web").click();
    settle(&mut h);
    assert!(h.state().submitted().iter().rev().any(|c| c.id == "session_focus"));
    assert!(layout(&h).minimized.is_empty());
    assert!(has(&h, "Pane: Odulodo.Web"));
}

#[test]
fn close_goes_through_the_busy_check() {
    let mut h = h();
    // Odulodo waits on an approval: Close asks first and closes nothing yet.
    assert_eq!(h.state().model.session("odulodo").unwrap().status, Status::Approval);
    let before = h.state().submitted().len();
    let p = control(&h, "Odulodo", "Close session");
    click_at(&mut h, p);
    assert!(calls_since(&h, before, "session_close").is_empty(), "asked first");
    assert_eq!(h.state().ui.overlay, csm_ui::app::Overlay::Confirm);
    assert!(h.query_by_label_contains("Keep it").is_some());
    h.get_by_label_contains("Close anyway").click();
    settle(&mut h);
    assert_eq!(calls_since(&h, before, "session_close"), vec![json!({"session_id": "odulodo", "force": true})]);
    assert!(h.state().model.session("odulodo").is_none());

    // An idle session closes at once (the engine's guard still applies: force is false).
    let mut h2 = self::h();
    let ctx = h2.ctx.clone();
    h2.state_mut().run(&ctx, Action::Tool("demo_set_status", json!({"session_id": "webps", "status": "idle"})));
    settle(&mut h2);
    let before = h2.state().submitted().len();
    let p = control(&h2, "Odulodo.Web (PS)", "Close session");
    click_at(&mut h2, p);
    assert_eq!(calls_since(&h2, before, "session_close"), vec![json!({"session_id": "webps", "force": false})]);

    // Every other UI close goes the same way: Ctrl+W on a busy active session asks first.
    let mut h3 = self::h();
    let ctx = h3.ctx.clone();
    h3.state_mut().run(&ctx, Action::Tool("session_focus", json!({"session_id": "odulodo"})));
    settle(&mut h3);
    let before = h3.state().submitted().len();
    h3.state_mut().run(&ctx, Action::CloseSession("odulodo".into()));
    settle(&mut h3);
    assert!(calls_since(&h3, before, "session_close").is_empty(), "the ⋯ menu / Ctrl+W close asks first too");
    assert_eq!(h3.state().ui.overlay, csm_ui::app::Overlay::Confirm);

    // With the warning off (Settings), a busy session closes at once, forced.
    let mut h4 = self::h();
    h4.state_mut().prefs.warn_on_close_when_busy = false;
    let before = h4.state().submitted().len();
    let p = control(&h4, "Odulodo", "Close session");
    click_at(&mut h4, p);
    assert_eq!(calls_since(&h4, before, "session_close"), vec![json!({"session_id": "odulodo", "force": true})]);
    assert_ne!(h4.state().ui.overlay, csm_ui::app::Overlay::Confirm);
}

#[test]
fn every_layout_mode_has_the_window_bar_and_narrow_headers_do_not_overflow() {
    let mut h = h();
    for mode in [LayoutMode::Side, LayoutMode::Grid, LayoutMode::Free, LayoutMode::Overview, LayoutMode::Focus] {
        let key = scope_key(&h);
        let ctx = h.ctx.clone();
        let params = if mode == LayoutMode::Grid {
            json!({"group": key, "mode": "grid", "cols": 3, "rows": 3})
        } else {
            json!({"group": key, "mode": mode.id()})
        };
        h.state_mut().run(&ctx, Action::Tool("layout_set", params));
        settle(&mut h);
        h.state_mut().run(&ctx, Action::CloseMenu);
        settle(&mut h);
        let n = panes(&h);
        assert!(n >= 1, "{mode:?}");
        assert_eq!(h.query_all_by_label("Close session").count(), n, "{mode:?}: a Close on every pane");
        assert_eq!(h.query_all_by_label("Minimize").count(), n, "{mode:?}: a Minimize on every pane");
        let max = h.query_all_by_label("Maximize").count();
        assert_eq!(max, if mode == LayoutMode::Focus { 0 } else { n }, "{mode:?}: Maximize (not in Focus, where one pane fills it)");
        // Every header's controls stay inside it and leave the name its room.
        for head in h.query_all_by_label_contains("Pane: ").map(|n| n.rect()).collect::<Vec<_>>() {
            let row = head.expand2(vec2(size::SPLIT_GRIP, 0.0));
            for label in ["Close session", "Minimize", "Maximize", "More  (Ctrl+.)"] {
                for r in h.query_all_by_label(label).map(|n| n.rect()).filter(|r| row.contains(r.center())) {
                    assert!(r.max.x <= head.max.x + size::SPLIT_GRIP, "{mode:?} {label} inside the header");
                    assert!(r.min.x >= head.min.x + size::PANE_NAME_MIN_W, "{mode:?} {label} leaves the name its room");
                }
            }
        }
    }
}

#[test]
fn a_torn_off_pane_shows_dock_and_close_only() {
    let mut h = h();
    h.get_by_label("Layout: Free").click();
    settle(&mut h);
    let ctx = h.ctx.clone();
    let key = scope_key(&h);
    h.state_mut().run(&ctx, Action::Tool("pane_tear_off", json!({"session_id": "webps", "group": key})));
    for _ in 0..5 {
        settle(&mut h);
    }
    assert!(has(&h, "Dock back into the main window"));
    let n = panes(&h); // the main window's panes and the torn-off one
    assert_eq!(h.query_all_by_label("Close session").count(), n, "Close everywhere");
    assert_eq!(h.query_all_by_label("Minimize").count(), n - 1, "no Minimize in its own window");
    assert_eq!(h.query_all_by_label("Maximize").count(), n - 1, "no Maximize in its own window");
}

#[test]
fn the_window_bar_tools_are_in_the_palette_and_the_pane_menu() {
    let h = h();
    let reg = h.state().dispatch.registry();
    for id in ["pane_minimize", "pane_maximize", "pane_restore"] {
        let spec = reg.get(id).unwrap_or_else(|| panic!("{id} registered"));
        assert_eq!(spec.effects, csm_engine::Effects::UI, "{id}: UI state only");
    }
    let menu = csm_ui::screens::menus::pane_menu(&h.state().model, "web");
    let labels: Vec<String> = menu
        .iter()
        .filter_map(|e| match e {
            csm_ui::screens::menus::Entry::Item { label, .. } => Some(label.clone()),
            _ => None,
        })
        .collect();
    for want in ["Minimize", "Maximize", "Float in this window", "Move to a new window"] {
        assert!(labels.iter().any(|l| l == want), "{want} in {labels:?}");
    }
}

#[test]
fn every_layout_button_stays_visible_on_a_narrow_window() {
    // A laptop-sized window: the layout row would run under the Inspector button, which hid
    // Board (reported from a laptop). It must shrink to icons instead, and still click.
    for width in [1280.0, 1000.0, 860.0] {
        let mut h = Harness::builder()
            .with_size(vec2(width, 760.0))
            .with_step_dt(1.0 / 60.0)
            .build_ui_state(|ui, app: &mut csm_ui::CsmApp| app.frame(ui), app_with(backend(ThemeKind::Mocha)));
        settle(&mut h);
        let inspector = h.get_by_label("Inspector").rect();
        for m in LayoutMode::ALL {
            let b = h.get_by_label(&format!("Layout: {}", m.label())).rect();
            assert!(b.max.x <= inspector.min.x, "{width}px: Layout: {} ({b:?}) runs under Inspector ({inspector:?})", m.label());
            assert!(b.max.x <= width && b.width() > 0.0, "{width}px: Layout: {} is off screen ({b:?})", m.label());
        }
        let board = h.get_by_label("Layout: Board").rect().center();
        click_at(&mut h, board);
        settle(&mut h);
        assert_eq!(layout(&h).mode, LayoutMode::Board, "{width}px");
    }
}

#[test]
fn the_palette_finds_each_layout_by_name() {
    // "board" in Ctrl+K must reach the Board (it only offered a cycle-to-next layout command).
    let mut h = h();
    let ctx = h.ctx.clone();
    h.state_mut().run(&ctx, Action::OpenPalette);
    settle(&mut h);
    h.event(Event::Text("board".into()));
    settle(&mut h);
    let first = csm_ui::screens::palette::rows(h.state(), "board").into_iter().find_map(|r| match r {
        csm_ui::screens::palette::PalRow::Tool { id, .. } => Some(id),
        _ => None,
    });
    assert_eq!(first, Some("layout:board"));
    // Enter would take the first row ("search output for board"), so pick the row itself.
    h.get_by_label("Palette: Layout: Board").click();
    settle(&mut h);
    assert_eq!(layout(&h).mode, LayoutMode::Board);
}
