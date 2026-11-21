//! Add-on screens in the window, through the real UI (egui_kittest + AccessKit) on the demo
//! backend, whose "plugin" is a handle the test changes: the view strip, the frame CSM draws, a
//! click that sends an event, a typed draft that survives a re-render, a button bound to a tool,
//! the confirm for `session_send_text`, a refused tool drawn disabled, the file list's folders,
//! invisible characters shown as markers, the pop-out window, and consent cards above screens.
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

fn tree(rev: u64, root: Value) -> Arc<ScreenTree> {
    Arc::new(ScreenTree::from_render(&json!({"rev": rev, "root": root}), "echo-test", None).unwrap_or_else(|e| panic!("{e}")))
}

fn info(instance: &str, phase: ScreenPhase) -> ScreenInfo {
    ScreenInfo {
        instance: instance.into(),
        addon: "echo-test".into(),
        addon_name: "Echo test plugin".into(),
        screen: "echo-test.main".into(),
        title: "Echo screen".into(),
        icon: "layers".into(),
        scope: ScreenScope::Global,
        phase,
        popped_out: false,
        error: None,
        rev: 1,
        context: Default::default(),
    }
}

fn demo_view() -> Value {
    json!({"type": "column", "key": "root", "gap": "md", "children": [
        {"type": "text", "key": "status", "text": "Hello from the add-on"},
        {"type": "toolbar", "children": [
            {"type": "button", "key": "btn", "label": "Ping", "kind": "primary", "event": true},
            {"type": "button", "label": "Run own tool", "action": {"tool": "echo-test.go", "params": {"a": 1}}},
            {"type": "button", "label": "Send hi", "action": {"tool": "session_send_text", "params": {"session_id": "odulodo", "text": "hello from the screen"}}},
            {"type": "button", "label": "Quit app", "action": {"tool": "app_quit"}},
            {"type": "button", "label": "Enable thing", "action": {"tool": "addon_enable", "params": {"id": "x"}}}
        ]},
        {"type": "text_input", "key": "name", "value": "", "placeholder": "Name", "submit_label": "Go"}
    ]})
}

/// An app with one open screen showing `root`, selected in the view strip.
fn app_with_screen(root: Value) -> (H, Handle) {
    let b = backend(ThemeKind::Mocha);
    let handle = b.screens_handle();
    {
        let mut g = handle.lock().unwrap();
        g.declared = vec![DeclaredScreen {
            addon: "echo-test".into(),
            addon_name: "Echo test plugin".into(),
            id: "echo-test.main".into(),
            title: "Echo screen".into(),
            icon: "layers".into(),
            scope: ScreenScope::Global,
            summary: String::new(),
        }];
        g.instances = vec![info("i1", ScreenPhase::Ready)];
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

fn events(handle: &Handle) -> Vec<(String, csm_core::addons::screen::ScreenEvent)> {
    handle.lock().unwrap().events.clone()
}

fn has(h: &H, label: &str) -> bool {
    h.query_by_label(label).is_some()
}

#[test]
fn the_view_strip_appears_with_a_screen_and_the_frame_names_the_add_on() {
    let b = backend(ThemeKind::Mocha);
    let handle = b.screens_handle();
    let mut h = harness(app_with(b));
    settle(&mut h);
    assert!(!has(&h, "View: Sessions"), "no screen open: the main area looks as it did");
    {
        let mut g = handle.lock().unwrap();
        g.instances = vec![info("i1", ScreenPhase::Ready)];
        g.trees.insert("i1".into(), tree(1, demo_view()));
    }
    settle(&mut h);
    assert!(has(&h, "View: Sessions") && has(&h, "View: Echo screen"), "the strip: Sessions and a tab for the screen");
    // Sessions is still showing until the screen is picked.
    assert!(!has(&h, "Hello from the add-on"));
    h.get_by_label("View: Echo screen").click();
    settle(&mut h);
    assert!(has(&h, "Add-on screen: Echo screen (Echo test plugin)"), "the frame's header names the add-on");
    assert!(has(&h, "Hello from the add-on"));
    assert!(has(&h, "Pop out into its own window"));
    h.get_by_label("View: Sessions").click();
    settle(&mut h);
    assert!(!has(&h, "Hello from the add-on"), "back on the sessions");
    // Closing is a tool call.
    h.get_by_label("Close Echo screen").click();
    settle(&mut h);
    assert_eq!(calls(&h, "screen_close"), vec![json!({"instance": "i1"})]);
    assert!(!has(&h, "View: Echo screen"), "the demo applied the close, and the strip is gone");
}

#[test]
fn a_click_sends_a_screen_event_and_a_typed_draft_survives_a_re_render() {
    let (mut h, handle) = app_with_screen(demo_view());
    h.get_by_label("Ping").click();
    settle(&mut h);
    let sent = events(&handle);
    assert_eq!(sent.len(), 1);
    assert_eq!((sent[0].0.as_str(), sent[0].1.key.as_str(), sent[0].1.kind), ("i1", "btn", EventKind::Click));
    // Type into the input.
    h.get_by_label("Name").click();
    h.step();
    h.get_by_label("Name").type_text("Ada");
    settle(&mut h);
    assert_eq!(lock(&h.state().ui.screens).draft("i1", "name"), Some("Ada"));
    // The plugin re-renders (same input, value still ""): the draft is not overwritten.
    handle.lock().unwrap().trees.insert("i1".into(), tree(2, demo_view()));
    handle.lock().unwrap().instances[0].rev = 2;
    settle(&mut h);
    assert_eq!(lock(&h.state().ui.screens).draft("i1", "name"), Some("Ada"), "typing never waits for a round trip");
    // Submit sends the typed text.
    h.get_by_label("Go").click();
    settle(&mut h);
    let sent = events(&handle);
    let submit = sent.iter().find(|(_, e)| e.kind == EventKind::Submit).expect("a submit event");
    assert_eq!((submit.1.key.as_str(), &submit.1.value), ("name", &json!({"value": "Ada"})));
    // After a successful submit the plugin sends `reset`, which does clear it.
    let mut v = demo_view();
    v["children"][2]["reset"] = json!(true);
    handle.lock().unwrap().trees.insert("i1".into(), tree(3, v));
    handle.lock().unwrap().instances[0].rev = 3;
    settle(&mut h);
    assert_eq!(lock(&h.state().ui.screens).draft("i1", "name"), Some(""));
}

#[test]
fn a_busy_plugin_refuses_the_event_with_a_toast() {
    let (mut h, handle) = app_with_screen(demo_view());
    handle.lock().unwrap().refuse_events = Some("Echo test plugin is busy; try again in a moment".into());
    h.get_by_label("Ping").click();
    settle(&mut h);
    assert!(has(&h, "Echo test plugin is busy; try again in a moment"), "the UI never waits; it says so");
    assert!(events(&handle).is_empty());
}

#[test]
fn a_button_bound_to_the_add_ons_own_tool_runs_it_directly() {
    let (mut h, _) = app_with_screen(demo_view());
    h.get_by_label("Run own tool").click();
    settle(&mut h);
    assert_eq!(calls(&h, "echo-test.go"), vec![json!({"a": 1})]);
}

#[test]
fn session_send_text_from_a_button_asks_first_with_the_exact_text() {
    let (mut h, _) = app_with_screen(demo_view());
    h.get_by_label("Send hi").click();
    settle(&mut h);
    assert!(calls(&h, "session_send_text").is_empty(), "nothing runs before the answer");
    // The confirm is CSM's: it names the add-on, the tool, the exact text and the target session.
    assert!(h.query_by_label_contains("Echo test plugin wants to paste text into a session").is_some());
    let body = h.get_by_label_contains("Exact text:").accesskit_node().value().unwrap_or_default();
    assert!(body.contains("hello from the screen"), "{body}");
    assert!(body.contains("Odulodo (odulodo)"), "the target session by name: {body}");
    assert!(body.contains("echo-test"), "the add-on's id: {body}");
    h.get_by_label("Cancel").click();
    settle(&mut h);
    assert!(calls(&h, "session_send_text").is_empty());
    // Once more, and confirm: now it is a tool call with those exact params.
    h.get_by_label("Send hi").click();
    settle(&mut h);
    h.get_by_label("Run it").click();
    settle(&mut h);
    assert_eq!(calls(&h, "session_send_text"), vec![json!({"session_id": "odulodo", "text": "hello from the screen"})]);
}

#[test]
fn a_tool_that_is_not_allowed_draws_disabled_and_runs_nothing() {
    let (mut h, _) = app_with_screen(demo_view());
    for label in ["Quit app", "Enable thing"] {
        let n = h.get_by_label(label);
        assert!(n.accesskit_node().is_disabled(), "{label} must draw disabled");
        n.click();
        settle(&mut h);
    }
    assert!(calls(&h, "app_quit").is_empty() && calls(&h, "addon_enable").is_empty());
    assert!(!h.query_by_label("Ping").unwrap().accesskit_node().is_disabled(), "an allowed button is enabled");
}

#[test]
fn file_list_builds_folders_from_the_paths_and_folds_them() {
    let root = json!({"type": "file_list", "key": "files", "view": "tree", "files": [
        {"path": "src/lib.rs", "status": "M"}, {"path": "src/ui/a.rs", "status": "A"}, {"path": "README.md", "status": "?"}]});
    let (mut h, handle) = app_with_screen(root);
    for l in ["Folder: src", "Folder: src/ui", "File: src/lib.rs", "File: src/ui/a.rs", "File: README.md"] {
        assert!(has(&h, l), "{l}");
    }
    h.get_by_label("Folder: src").click();
    settle(&mut h);
    assert!(!has(&h, "File: src/lib.rs") && !has(&h, "Folder: src/ui"), "a folded folder hides its children");
    assert!(events(&handle).is_empty(), "folding is local: no event for the plugin");
    h.get_by_label("Folder: src").click();
    settle(&mut h);
    h.get_by_label("File: src/lib.rs").click();
    settle(&mut h);
    let sent = events(&handle);
    assert_eq!((sent[0].1.key.as_str(), sent[0].1.kind, &sent[0].1.value), ("files", EventKind::Select, &json!({"path": "src/lib.rs"})));
}

#[test]
fn lists_tabs_and_segmented_controls_send_select_events() {
    let root = json!({"type": "column", "children": [
        {"type": "segmented", "key": "src", "options": [{"value": "w", "label": "Working"}, {"value": "s", "label": "Staged"}], "selected": "w"},
        {"type": "list", "key": "items", "items": [{"key": "one", "title": "One", "badges": ["3"]}, {"key": "two", "title": "Two"}]},
        {"type": "tabs", "key": "tabs", "tabs": [{"key": "a", "label": "Alpha"}, {"key": "b", "label": "Beta"}], "selected": "a", "child": {"type": "text", "text": "tab body"}}
    ]});
    let (mut h, handle) = app_with_screen(root);
    h.get_by_label("src: Staged").click();
    h.get_by_label("Item: Two").click();
    h.get_by_label("Tab: Beta").click();
    settle(&mut h);
    let sent: Vec<(String, Value)> = events(&handle).into_iter().map(|(_, e)| (e.key, e.value)).collect();
    assert_eq!(sent, vec![("src".into(), json!({"value": "s"})), ("items".into(), json!({"key": "two"})), ("tabs".into(), json!({"key": "b"}))]);
    assert!(has(&h, "tab body"));
}

#[test]
fn invisible_characters_are_drawn_as_visible_markers() {
    let root = json!({"type": "column", "children": [
        {"type": "text", "text": "let admin = \"\u{202E}nimda\"; \u{1b}[2J"},
        {"type": "markdown", "source": "see [here](javascript:alert(1)) and [site](https://example.com/x) and [bank](https://evil.example/)"},
        {"type": "badge", "text": "a\u{202E}b"},
        {"type": "button", "label": "Go\u{200B}now", "event": true, "key": "b"}
    ]});
    let (mut h, handle) = app_with_screen(root);
    assert!(h.query_by_label_contains("\u{27E8}U+202E\u{27E9}nimda").is_some(), "bidi override shown, not applied");
    assert!(h.query_by_label_contains("\u{241B}[2J").is_some(), "ESC shown, not sent anywhere");
    assert!(has(&h, "Badge: ab"), "labels drop them");
    assert!(has(&h, "Gonow"));
    // A javascript: link is plain text; an https one asks open_url as the person's click.
    assert!(!has(&h, "Link: here"));
    h.get_by_label("Link: site").click();
    settle(&mut h);
    assert_eq!(calls(&h, "open_url"), vec![json!({"url": "https://example.com/x"})]);
    assert!(events(&handle).is_empty());
}

#[test]
fn unknown_widgets_draw_as_a_small_unsupported_box() {
    let (h, _) = app_with_screen(json!({"type": "column", "children": [{"type": "hologram", "patch": "x"}, {"type": "text", "text": "after"}]}));
    assert!(has(&h, "unsupported: hologram") && has(&h, "after"));
}

#[test]
fn the_frame_and_its_header_are_there_in_every_state() {
    for (phase, error, tree_present, want) in [
        (ScreenPhase::Opening, None, false, "Opening Echo screen\u{2026}"),
        (ScreenPhase::NotAnswering, Some("Echo test plugin did not answer within 10 seconds"), false, "Echo test plugin did not answer."),
        (ScreenPhase::Stopped, None, true, "Echo test plugin is not running."),
        (ScreenPhase::Stopped, None, false, "Echo test plugin is not running."),
        (ScreenPhase::Ready, Some("the tree has more than 5000 nodes"), true, "Echo test plugin sent a view CSM could not show: the tree has more than 5000 nodes"),
    ] {
        let (mut h, handle) = app_with_screen(demo_view());
        {
            let mut g = handle.lock().unwrap();
            g.instances[0].phase = phase;
            g.instances[0].error = error.map(str::to_owned);
            if !tree_present {
                g.trees.clear();
            }
        }
        settle(&mut h);
        assert!(has(&h, "Add-on screen: Echo screen (Echo test plugin)"), "{phase:?}: the frame names the add-on");
        assert!(h.query_by_label_contains(want).is_some(), "{phase:?}: {want}");
        if tree_present {
            assert!(has(&h, "Hello from the add-on"), "{phase:?}: the last good tree stays");
        }
    }
}

#[test]
fn a_stopped_add_on_offers_restart_and_close_and_its_tree_is_greyed_out() {
    let (mut h, handle) = app_with_screen(demo_view());
    handle.lock().unwrap().instances[0].phase = ScreenPhase::Stopped;
    settle(&mut h);
    assert!(h.get_by_label("Ping").accesskit_node().is_disabled(), "a stopped add-on's buttons do nothing");
    h.get_by_label("Restart").click();
    settle(&mut h);
    assert_eq!(calls(&h, "plugin_restart"), vec![json!({"id": "echo-test"})]);
    h.get_by_label("Close").click();
    settle(&mut h);
    assert_eq!(calls(&h, "screen_close"), vec![json!({"instance": "i1"})]);
}

#[test]
fn a_screen_that_did_not_answer_offers_retry() {
    let (mut h, handle) = app_with_screen(demo_view());
    {
        let mut g = handle.lock().unwrap();
        g.trees.clear();
        g.instances[0].phase = ScreenPhase::NotAnswering;
        g.instances[0].context.project = Some("repo:x".into());
    }
    settle(&mut h);
    h.get_by_label("Retry").click();
    settle(&mut h);
    let c = calls(&h, "screen_open");
    assert_eq!(c.len(), 1);
    assert_eq!(c[0]["screen"], "echo-test.main");
    assert_eq!(c[0]["context"]["project"], "repo:x");
}

#[test]
fn pop_out_and_dock_are_tool_calls_and_the_window_holds_the_screen() {
    let (mut h, handle) = app_with_screen(demo_view());
    h.get_by_label("Pop out into its own window").click();
    settle(&mut h);
    assert_eq!(calls(&h, "screen_pop_out"), vec![json!({"instance": "i1"})]);
    assert!(handle.lock().unwrap().instances[0].popped_out, "the demo applied it");
    h.step();
    h.step();
    // The tab stays in the strip; the screen is in its own window now (embedded in the harness).
    assert!(has(&h, "View: Echo screen"));
    assert!(has(&h, "Dock back into the main window"), "the window's header offers Dock");
    h.get_by_label("Dock back into the main window").click();
    settle(&mut h);
    assert_eq!(calls(&h, "screen_dock"), vec![json!({"instance": "i1"})]);
    assert!(!handle.lock().unwrap().instances[0].popped_out);
}

#[test]
fn consent_cards_are_drawn_above_screens() {
    let (mut h, _) = app_with_screen(demo_view());
    h.state_mut().ui.consents.push(json!({"request_id": 7, "client": "add-on echo-test", "tool": "session_list", "title": "List sessions", "summary": "s", "params": null}));
    settle(&mut h);
    assert!(has(&h, "Allow once"), "the card shows over the screen");
    let ctx = h.ctx.clone();
    let layers: Vec<egui::LayerId> = ctx.memory(|m| m.areas().visible_layer_ids().into_iter().collect());
    let card = layers.iter().find(|l| l.id == egui::Id::new(("consent", 7u64))).expect("the card's layer");
    assert!(card.order > egui::Order::Middle, "a consent card is Foreground, above every screen (which draws at Background or Middle)");
    // And a click on it reaches the card, not the screen below.
    h.get_by_label("Allow once").click();
    settle(&mut h);
    assert_eq!(calls(&h, "consent_answer").len(), 1);
    assert!(events_none(&h));
}

fn events_none(h: &H) -> bool {
    lock(&h.state().ui.screens).sent().is_empty()
}

#[test]
fn it_draws_in_both_themes_and_at_a_narrow_width() {
    for kind in [ThemeKind::Mocha, ThemeKind::Latte] {
        let b = backend(kind);
        let handle = b.screens_handle();
        {
            let mut g = handle.lock().unwrap();
            g.instances = vec![info("i1", ScreenPhase::Ready)];
            g.trees.insert("i1".into(), tree(1, demo_view()));
        }
        let mut h = harness(app_with(b));
        settle(&mut h);
        lock(&h.state().ui.screens).active = Some("i1".into());
        settle(&mut h);
        assert!(has(&h, "Hello from the add-on"), "{kind:?}");
        h.set_size(egui::vec2(780.0, 520.0));
        settle(&mut h);
        assert!(has(&h, "Add-on screen: Echo screen (Echo test plugin)"), "{kind:?} narrow");
    }
}

#[test]
fn the_palette_offers_an_open_row_per_declared_screen() {
    let b = backend(ThemeKind::Mocha);
    let handle = b.screens_handle();
    handle.lock().unwrap().declared = vec![DeclaredScreen {
        addon: "echo-test".into(),
        addon_name: "Echo test plugin".into(),
        id: "echo-test.main".into(),
        title: "Echo screen".into(),
        icon: "layers".into(),
        scope: ScreenScope::Global,
        summary: String::new(),
    }];
    let mut h = harness(app_with(b));
    settle(&mut h);
    let rows = csm_ui::screens::palette::rows(h.state(), ">");
    assert!(rows.iter().any(|r| matches!(r, csm_ui::screens::palette::PalRow::Screen { id, title, .. } if id == "echo-test.main" && title == "Open Echo screen")));
    let typed = csm_ui::screens::palette::rows(h.state(), "echo scr");
    assert!(typed.iter().any(|r| matches!(r, csm_ui::screens::palette::PalRow::Screen { .. })));
}

