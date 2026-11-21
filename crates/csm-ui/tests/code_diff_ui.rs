//! The `code` and `diff` widgets of add-on screens, through the real UI (egui_kittest + AccessKit)
//! on the demo backend: line numbers, a line-range selection (click, shift-click, keys), range
//! actions that send `range_action`, an open thread drawn under its last line, a closed thread's
//! badge, gutter marks, unified and side-by-side diffs, hostile text shown as markers, too-large
//! notices, a 120,000-line file, highlighting that never runs on the UI thread, and screenshots in
//! both themes.
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

mod common;

use std::sync::mpsc::sync_channel;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use common::*;
use csm_core::addons::screen::{EventKind, ScreenScope, ScreenTree};
use csm_core::diff::Side;
use csm_engine::{DeclaredScreen, ScreenInfo, ScreenPhase};
use csm_ui::demo::DemoScreens;
use csm_ui::terminal::lock;
use csm_ui::theme::ThemeKind;
use egui::{Key, Modifiers};
use egui_kittest::Harness;
use egui_kittest::kittest::{NodeT, Queryable};
use serde_json::{Value, json};

type H = Harness<'static, csm_ui::CsmApp>;
type Handle = Arc<Mutex<DemoScreens>>;

fn tree(rev: u64, root: Value) -> Arc<ScreenTree> {
    Arc::new(ScreenTree::from_render(&json!({"rev": rev, "root": root}), "echo-test", None).unwrap_or_else(|e| panic!("{e}")))
}

fn info(instance: &str) -> ScreenInfo {
    ScreenInfo {
        instance: instance.into(),
        addon: "echo-test".into(),
        addon_name: "Echo test plugin".into(),
        screen: "echo-test.main".into(),
        title: "Review".into(),
        icon: "layers".into(),
        scope: ScreenScope::Global,
        phase: ScreenPhase::Ready,
        popped_out: false,
        error: None,
        rev: 1,
        context: Default::default(),
    }
}

fn open_with(theme: ThemeKind, root: Value, wgpu: bool) -> (H, Handle) {
    let b = backend(theme);
    let handle = b.screens_handle();
    {
        let mut g = handle.lock().unwrap();
        g.declared = vec![DeclaredScreen {
            addon: "echo-test".into(),
            addon_name: "Echo test plugin".into(),
            id: "echo-test.main".into(),
            title: "Review".into(),
            icon: "layers".into(),
            scope: ScreenScope::Global,
            summary: String::new(),
        }];
        g.instances = vec![info("i1")];
        g.trees.insert("i1".into(), tree(1, root));
    }
    let mut h = if wgpu { harness_wgpu(app_with(b), 1.0) } else { harness(app_with(b)) };
    settle(&mut h);
    lock(&h.state().ui.screens).active = Some("i1".into());
    settle(&mut h);
    (h, handle)
}

fn app_with_screen(root: Value) -> (H, Handle) {
    open_with(ThemeKind::Mocha, root, false)
}

fn has(h: &H, label: &str) -> bool {
    h.query_by_label(label).is_some()
}

fn has_part(h: &H, part: &str) -> bool {
    h.query_by_label_contains(part).is_some()
}

fn events(handle: &Handle) -> Vec<csm_core::addons::screen::ScreenEvent> {
    handle.lock().unwrap().events.iter().map(|(_, e)| e.clone()).collect()
}

fn selection(h: &H, key: &str) -> Option<(Side, u32, u32)> {
    lock(&h.state().ui.screens).selection("i1", key)
}

/// Frames until the highlight worker has nothing pending (or 20 s).
fn wait_highlighted(h: &mut H) {
    let end = Instant::now() + Duration::from_secs(20);
    loop {
        settle(h);
        if lock(&h.state().ui.screens).hl.pending() == 0 || Instant::now() > end {
            break;
        }
        std::thread::sleep(Duration::from_millis(10));
    }
    settle(h);
}

const RUST: &str = "use std::fmt;\n\n/// Adds.\nfn add(a: i32, b: i32) -> i32 {\n    // sum\n    let s = \"total\";\n    a + b + 42\n}\n\nfn main() {\n    println!(\"{}\", add(1, 2));\n}\n";

fn code_view(extra: Value) -> Value {
    let mut n = json!({"type": "code", "key": "code", "text": RUST, "language": "rust", "path": "src/lib.rs",
        "range_actions": [{"id": "comment", "label": "+ Comment"}]});
    n.as_object_mut().unwrap().extend(extra.as_object().unwrap().clone());
    n
}

const PATCH: &str = "diff --git a/src/lib.rs b/src/lib.rs\nindex 1111111..2222222 100644\n--- a/src/lib.rs\n+++ b/src/lib.rs\n@@ -1,6 +1,7 @@ fn main() {\n fn keep() {}\n-fn old() { 1 }\n+fn new() { 2 }\n+fn extra() {}\n let x = \"same\";\n // tail\n let a = \"alpha beta gamma\";\n let b = \"delta\";\n";

fn diff_view(split: bool, extra: Value) -> Value {
    let mut n = json!({"type": "diff", "key": "diff", "patch": PATCH, "view": if split { "split" } else { "unified" }, "language": "rust",
        "range_actions": [{"id": "comment", "label": "+ Comment"}]});
    n.as_object_mut().unwrap().extend(extra.as_object().unwrap().clone());
    n
}

fn range_actions(handle: &Handle) -> Vec<Value> {
    events(handle).into_iter().filter(|e| e.kind == EventKind::RangeAction).map(|e| e.value).collect()
}

// ---------------------------------------------------------------------------------------------

#[test]
fn a_code_view_draws_numbered_lines_and_a_click_selects_one() {
    let (mut h, handle) = app_with_screen(code_view(json!({"first_line": 10})));
    for n in [10, 11, 12, 21] {
        assert!(has(&h, &format!("Line {n}")), "line {n} has a number");
    }
    assert!(!has(&h, "Line 9") && !has(&h, "Line 22"), "numbers start at first_line and stop with the text");
    assert!(has_part(&h, "fn add(a: i32, b: i32) -> i32 {"), "the text is there for a screen reader");
    assert!(!has(&h, "+ Comment"), "no range, no bar");
    h.get_by_label("Line 13").click();
    settle(&mut h);
    assert_eq!(selection(&h, "code"), Some((Side::New, 13, 13)));
    assert!(has(&h, "+ Comment"), "a range shows the plugin's range action in a bar CSM draws");
    assert!(events(&handle).is_empty(), "selecting is local: nothing is sent per click");
}

#[test]
fn shift_click_extends_the_range_and_the_range_action_sends_it() {
    let (mut h, handle) = app_with_screen(code_view(json!({})));
    h.get_by_label("Line 3").click();
    settle(&mut h);
    h.get_by_label("Line 5").click_modifiers(Modifiers::SHIFT);
    settle(&mut h);
    assert_eq!(selection(&h, "code"), Some((Side::New, 3, 5)), "shift-click selects 3 lines");
    h.get_by_label("+ Comment").click();
    settle(&mut h);
    assert_eq!(range_actions(&handle), vec![json!({"id": "comment", "side": "new", "start": 3, "end": 5})]);
    assert_eq!(events(&handle)[0].key, "code");
    assert_eq!(selection(&h, "code"), None, "the selection is cleared once it has been used");
    // Shift-click upward works too: the range is in order whichever way it was drawn.
    h.get_by_label("Line 8").click();
    settle(&mut h);
    h.get_by_label("Line 6").click_modifiers(Modifiers::SHIFT);
    settle(&mut h);
    assert_eq!(selection(&h, "code"), Some((Side::New, 6, 8)));
}

#[test]
fn shift_arrows_extend_the_range_and_escape_clears_it() {
    let (mut h, _) = app_with_screen(code_view(json!({})));
    h.get_by_label("Line 2").click();
    settle(&mut h);
    h.key_press_modifiers(Modifiers::SHIFT, Key::ArrowDown);
    h.key_press_modifiers(Modifiers::SHIFT, Key::ArrowDown);
    settle(&mut h);
    assert_eq!(selection(&h, "code"), Some((Side::New, 2, 4)));
    h.key_press_modifiers(Modifiers::SHIFT, Key::ArrowUp);
    settle(&mut h);
    assert_eq!(selection(&h, "code"), Some((Side::New, 2, 3)));
    h.key_press(Key::Escape);
    settle(&mut h);
    assert_eq!(selection(&h, "code"), None);
}

#[test]
fn an_open_thread_draws_under_its_last_line_and_a_closed_one_is_a_badge() {
    let thread = |key: &str, open: bool, end: u64| {
        json!({"key": key, "anchor": {"start": 4, "end": end}, "open": open, "count": 2, "child": {"type": "column", "key": format!("{key}-body"), "children": [
            {"type": "comment", "author": {"name": "Allan", "kind": "person"}, "time_ms": 1, "body": format!("note {key}")},
            {"type": "comment", "author": {"name": "Claude", "kind": "agent"}, "time_ms": 2, "body": "fixed"},
            {"type": "text_input", "key": format!("{key}-in"), "value": "", "placeholder": "Reply", "submit_label": "Send"}]}})
    };
    let (mut h, handle) = app_with_screen(code_view(json!({"threads": [thread("t1", true, 6), thread("t2", false, 8)]})));
    assert!(has_part(&h, "note t1"), "an open thread draws its child");
    assert!(has(&h, "Reply"), "with its input");
    let (l6, l7, body) = (h.get_by_label("Line 6").rect(), h.get_by_label("Line 7").rect(), h.get_by_label_contains("note t1").rect());
    assert!(body.min.y >= l6.max.y - 1.0, "the thread is under line 6: {body:?} vs {l6:?}");
    assert!(l7.min.y >= body.max.y - 1.0, "and pushes line 7 below it: {l7:?} vs {body:?}");
    assert!(!has_part(&h, "note t2"), "a closed thread draws no block");
    h.get_by_label("Open thread (2)").click();
    settle(&mut h);
    let ev = events(&handle);
    assert_eq!((ev[0].kind, ev[0].key.as_str(), &ev[0].value), (EventKind::ThreadToggle, "code", &json!({"key": "t2"})));
    // The reply typed into a thread goes only to its owner, on submit.
    h.get_by_label("Reply").click();
    h.step();
    h.get_by_label("Reply").type_text("thanks");
    settle(&mut h);
    h.get_by_label("Send").click();
    settle(&mut h);
    let ev = events(&handle);
    let submit = ev.iter().find(|e| e.kind == EventKind::Submit).expect("a submit");
    assert_eq!((submit.key.as_str(), &submit.value), ("t1-in", &json!({"value": "thanks"})));
}

#[test]
fn marks_withheld_text_and_a_very_long_line() {
    let long = "x".repeat(50_000);
    let text = format!("short\n{long}\nlast\n");
    let (h, _) = app_with_screen(code_view(json!({"text": text, "marks": [{"line": 1, "tone": "danger"}, {"line": 3, "count": 4}], "highlight_lines": [3]})));
    assert!(has(&h, "Line 3"));
    let shown = h.get_all_by_label_contains("xxxxxxxx").next().expect("the long line is drawn");
    let label = shown.accesskit_node().label().unwrap_or_default();
    assert!(label.chars().count() <= csm_ui_max_cols() + 8, "a long line is cut: {} chars", label.chars().count());
    let big = "y".repeat(600 * 1024);
    let (h, _) = app_with_screen(code_view(json!({"text": big})));
    assert!(has_part(&h, "too large to show here"), "over 512 KiB is a notice, not a crash");
    assert!(!has(&h, "Line 1"));
    let (h, _) = app_with_screen(code_view(json!({"text": null, "blob": {"id": "b", "lines": 48210, "bytes": 3301120}})));
    assert!(has_part(&h, "48210 lines"));
    let (h, _) = app_with_screen(code_view(json!({"text": ""})));
    assert!(has(&h, "This file is empty."));
}

fn csm_ui_max_cols() -> usize {
    4_000
}

#[test]
fn invisible_and_bidi_characters_in_code_are_drawn_as_markers() {
    let text = "let s = \"\u{202E}nimda\"; // \u{1b}[2J\u{200B}\nplain\ttab\r\ncr\rmid\n";
    let (h, _) = app_with_screen(code_view(json!({"text": text})));
    assert!(has_part(&h, "\u{27E8}U+202E\u{27E9}nimda"), "a bidi override is shown, not applied");
    assert!(has_part(&h, "\u{241B}[2J\u{27E8}U+200B\u{27E9}"), "ESC and a zero-width space are shown");
    assert!(has_part(&h, "plain    tab"), "a tab is spaces");
    assert!(has_part(&h, "cr\u{240D}mid"), "a lone CR is shown; a CRLF is one line break");
    assert!(has(&h, "Line 3") && !has(&h, "Line 4"));
}

#[test]
fn a_unified_diff_shows_both_numbers_signs_and_selects_per_side() {
    let (mut h, handle) = app_with_screen(diff_view(false, json!({})));
    assert!(has_part(&h, "src/lib.rs") && has_part(&h, "+2 \u{2212}1"), "the file banner has the counts");
    assert!(has_part(&h, "@@ -1,6 +1,7 @@ fn main() {"));
    assert!(has(&h, "Removed: fn old() { 1 }") && has(&h, "Added: fn new() { 2 }") && has(&h, "Added: fn extra() {}"));
    assert!(has(&h, "Old line 2") && has(&h, "New line 2") && has(&h, "New line 3") && has(&h, "New line 4"));
    assert_eq!(h.get_all_by_label_contains("Old line").count(), 6, "the added lines have no old number");
    assert_eq!(h.get_all_by_label_contains("New line").count(), 7, "the deleted line has no new number");
    h.get_by_label("Old line 2").click();
    settle(&mut h);
    assert_eq!(selection(&h, "diff"), Some((Side::Old, 2, 2)));
    h.get_by_label("+ Comment").click();
    settle(&mut h);
    assert_eq!(range_actions(&handle), vec![json!({"id": "comment", "side": "old", "start": 2, "end": 2})]);
    h.get_by_label("New line 2").click();
    h.get_by_label("New line 4").click_modifiers(Modifiers::SHIFT);
    settle(&mut h);
    assert_eq!(selection(&h, "diff"), Some((Side::New, 2, 4)));
    // A shift-click on the other side starts a new selection instead of mixing sides.
    h.get_by_label("Old line 2").click_modifiers(Modifiers::SHIFT);
    settle(&mut h);
    assert_eq!(selection(&h, "diff"), Some((Side::Old, 2, 2)));
}

#[test]
fn a_split_diff_pairs_the_changed_lines_and_selects_one_side() {
    let (mut h, handle) = app_with_screen(diff_view(true, json!({})));
    assert!(has(&h, "Removed: fn old() { 1 }") && has(&h, "Added: fn new() { 2 }"));
    let (old, new) = (h.get_by_label("Removed: fn old() { 1 }").rect(), h.get_by_label("Added: fn new() { 2 }").rect());
    assert!((old.center().y - new.center().y).abs() < 2.0, "a deletion and its replacement share a row");
    assert!(new.min.x > old.max.x - 1.0, "old on the left, new on the right");
    h.get_by_label("New line 2").click();
    h.get_by_label("New line 3").click_modifiers(Modifiers::SHIFT);
    settle(&mut h);
    h.get_by_label("+ Comment").click();
    settle(&mut h);
    assert_eq!(range_actions(&handle), vec![json!({"id": "comment", "side": "new", "start": 2, "end": 3})]);
}

#[test]
fn threads_and_marks_anchor_to_a_side_of_a_diff() {
    let thread = json!({"key": "t", "anchor": {"side": "old", "start": 2, "end": 2}, "open": true, "child": {"type": "comment", "key": "c", "author": {"name": "A"}, "body": "why remove this"}});
    for split in [false, true] {
        let (h, _) = app_with_screen(diff_view(split, json!({"threads": [thread.clone()], "marks": [{"line": 3, "side": "new", "tone": "success"}]})));
        let (row, body) = (h.get_by_label("Removed: fn old() { 1 }").rect(), h.get_by_label_contains("why remove this").rect());
        assert!(body.min.y >= row.max.y - 1.0, "split={split}: the thread sits under the old line it is about");
    }
    // A thread on a line the diff does not show still draws, at the end.
    let lost = json!({"key": "t", "anchor": {"start": 999, "end": 999}, "open": true, "child": {"type": "text", "text": "elsewhere in the file"}});
    let (h, _) = app_with_screen(diff_view(false, json!({"threads": [lost]})));
    assert!(has_part(&h, "elsewhere in the file"));
}

#[test]
fn hostile_diffs_draw_without_panicking() {
    let long = "z".repeat(100_000);
    let patches = [
        "--- a/f\r\n+++ b/f\r\n@@ -1 +1 @@\r\n-old\u{1b}[31m\r\n+new\u{202e}\r\n".to_owned(),
        format!("--- a/f\n+++ b/f\n@@ -1 +1 @@\n-{long}\n+{long}x\n"),
        "diff --git a/img.png b/img.png\nBinary files a/img.png and b/img.png differ\n".to_owned(),
        "\u{0}\u{1}\u{2}\u{3}binary looking\u{fffd}\u{fffd}".repeat(50),
        "@@ -9999999999 +1,2 @@\n+x\n".to_owned(),
        String::new(),
        "not a diff at all".to_owned(),
    ];
    for (i, p) in patches.iter().enumerate() {
        for split in [false, true] {
            let (mut h, _) = app_with_screen(json!({"type": "diff", "key": "d", "patch": p, "view": if split { "split" } else { "unified" }}));
            settle(&mut h);
            h.step();
            assert!(has_part(&h, "Add-on screen: Review"), "patch {i} split={split}: the frame is still there");
        }
    }
    let (h, _) = app_with_screen(json!({"type": "diff", "key": "d", "patch": patches[0]}));
    assert!(has_part(&h, "Removed: old\u{241B}[31m") && has_part(&h, "Added: new\u{27E8}U+202E\u{27E9}"), "controls shown as markers, CRLF folded");
    let (h, _) = app_with_screen(json!({"type": "diff", "key": "d", "patch": patches[2]}));
    assert!(has(&h, "Binary file: not shown."));
    let (h, _) = app_with_screen(json!({"type": "diff", "key": "d", "patch": patches[5]}));
    assert!(has(&h, "No changes to show."));
    let (h, _) = app_with_screen(json!({"type": "diff", "key": "d", "patch": "x".repeat(600 * 1024)}));
    assert!(has_part(&h, "diff is too large to show here") && has_part(&h, "Open the file"));
}

#[test]
fn a_big_file_opens_fast_and_scroll_to_reaches_line_50000() {
    let text: String = "x\n".repeat(120_000);
    assert!(text.len() < 512 * 1024);
    let started = Instant::now();
    let (mut h, _) = app_with_screen(code_view(json!({"text": text, "language": "", "path": "", "scroll_to": 50_000})));
    for _ in 0..30 {
        h.step();
    }
    assert!(has(&h, "Line 50000"), "scroll_to brought line 50,000 into view");
    assert!(!has(&h, "Line 1"), "and only the visible rows are drawn");
    let per_frame = {
        let t = Instant::now();
        for _ in 0..10 {
            h.step();
        }
        t.elapsed() / 10
    };
    assert!(per_frame < Duration::from_millis(400), "a frame over 120,000 lines took {per_frame:?}");
    assert!(started.elapsed() < Duration::from_secs(60));
}

#[test]
fn highlighting_runs_on_a_worker_and_frames_never_wait_for_it() {
    let (open_tx, open_rx) = sync_channel::<()>(0);
    let open_rx = Mutex::new(open_rx);
    let calls: Arc<Mutex<Vec<std::thread::ThreadId>>> = Arc::default();
    let engine_calls = calls.clone();
    let (mut h, handle) = app_with_screen(json!({"type": "column", "children": [{"type": "text", "text": "x"}]}));
    let gate = csm_ui::screens::addon_screen::hl::Highlighter::with_engine(Arc::new(move |t: &str, l: &str, p: &str| {
        engine_calls.lock().unwrap().push(std::thread::current().id());
        let _ = open_rx.lock().map(|r| r.recv_timeout(Duration::from_secs(30)));
        csm_services::highlight::highlight(t, l, p)
    }));
    lock(&h.state().ui.screens).hl = gate;
    handle.lock().unwrap().trees.insert("i1".into(), tree(2, code_view(json!({}))));
    handle.lock().unwrap().instances[0].rev = 2;
    let started = Instant::now();
    for _ in 0..8 {
        h.step();
    }
    assert!(started.elapsed() < Duration::from_secs(10), "frames kept drawing while the engine was blocked");
    assert!(has(&h, "Line 1") && has_part(&h, "fn main() {"), "plain text is drawn until the spans arrive");
    assert_eq!(lock(&h.state().ui.screens).hl.pending(), 1);
    open_tx.send(()).unwrap();
    wait_highlighted(&mut h);
    let ids = calls.lock().unwrap().clone();
    assert_eq!(ids.len(), 1, "one document, one request, however many frames");
    assert_ne!(ids[0], std::thread::current().id(), "the engine ran off the UI thread");
}

// ---------------------------------------------------------------------------------------------
// Screenshots, both themes
// ---------------------------------------------------------------------------------------------

fn out_dir() -> std::path::PathBuf {
    let d = std::env::var_os("CSM_SCREENS_OUT")
        .map(std::path::PathBuf::from)
        .unwrap_or_else(|| std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("..").join("..").join("target").join("screens"));
    std::fs::create_dir_all(&d).unwrap();
    d
}

fn count_near(img: &egui::ColorImage, c: egui::Color32, tol: u8) -> usize {
    img.pixels.iter().filter(|p| p.r().abs_diff(c.r()) <= tol && p.g().abs_diff(c.g()) <= tol && p.b().abs_diff(c.b()) <= tol).count()
}

fn shoot(name: &str, theme: ThemeKind, root: Value, expect_syntax: bool) {
    let (mut h, _) = open_with(theme, root, true);
    wait_highlighted(&mut h);
    for _ in 0..3 {
        h.step();
    }
    let img = h.render().expect("render");
    img.save(out_dir().join(format!("{name}.png"))).unwrap();
    let t = csm_ui::theme::Tokens::for_kind(theme);
    let ci = egui::ColorImage::from_rgba_unmultiplied([img.width() as usize, img.height() as usize], img.as_raw());
    let distinct: std::collections::HashSet<[u8; 4]> = img.pixels().map(|p| p.0).collect();
    assert!(distinct.len() > 100, "{name} looks blank");
    if expect_syntax {
        assert!(count_near(&ci, t.syn_keyword, 6) > 20, "{name}: keywords are drawn in the keyword colour");
        assert!(count_near(&ci, t.syn_string, 6) > 20, "{name}: strings are drawn in the string colour");
        assert!(count_near(&ci, t.syn_comment, 6) > 10, "{name}: comments are drawn in the comment colour");
    }
}

fn diff_bg_over(t: &csm_ui::theme::Tokens, c: egui::Color32) -> egui::Color32 {
    // `c` is premultiplied: its colour already carries its alpha. Composite it over the code background.
    let a = u32::from(c.a());
    let over = |f: u8, b: u8| (u32::from(f) + u32::from(b) * (255 - a) / 255).min(255) as u8;
    egui::Color32::from_rgb(over(c.r(), t.bg.r()), over(c.g(), t.bg.g()), over(c.b(), t.bg.b()))
}

#[test]
fn code_and_diff_screenshots_in_both_themes() {
    for (theme, suffix) in [(ThemeKind::Mocha, "mocha"), (ThemeKind::Latte, "latte")] {
        let thread = json!({"key": "t1", "anchor": {"start": 4, "end": 4}, "open": true, "child": {"type": "column", "key": "t1b", "children": [
            {"type": "comment", "author": {"name": "Allan", "kind": "person"}, "time_ms": 1, "body": "Why `i32`?"},
            {"type": "comment", "author": {"name": "Claude", "kind": "agent"}, "time_ms": 2, "body": "Kept it; **fixed** the overflow."}]}});
        shoot(&format!("code-view-{suffix}"), theme, code_view(json!({"threads": [thread], "marks": [{"line": 7, "tone": "warning"}], "highlight_lines": [11]})), true);
        shoot(&format!("diff-unified-{suffix}"), theme, diff_view(false, json!({})), true);
        shoot(&format!("diff-split-{suffix}"), theme, diff_view(true, json!({})), true);
    }
    // The diff's add and delete bands are the tokens, in each theme.
    for theme in [ThemeKind::Mocha, ThemeKind::Latte] {
        let (mut h, _) = open_with(theme, diff_view(false, json!({})), true);
        wait_highlighted(&mut h);
        h.step();
        let img = h.render().expect("render");
        let ci = egui::ColorImage::from_rgba_unmultiplied([img.width() as usize, img.height() as usize], img.as_raw());
        let t = csm_ui::theme::Tokens::for_kind(theme);
        assert!(count_near(&ci, diff_bg_over(&t, t.diff_add_bg), 4) > 2000, "{theme:?}: added lines have the add band");
        assert!(count_near(&ci, diff_bg_over(&t, t.diff_del_bg), 4) > 1000, "{theme:?}: deleted lines have the delete band");
    }
}

#[test]
fn file_and_hunk_banners_show_invisible_characters_as_markers() {
    // A file name with a right-to-left override and a heading with an escape: shown, not applied.
    let patch = "--- a/x\u{202e}y.rs\n+++ b/x\u{202e}y.rs\n@@ -1 +1 @@ fn \u{1b}[2J()\n-a\n+b\n";
    let (h, _) = app_with_screen(diff_view(false, json!({"patch": patch})));
    assert!(has_part(&h, "\u{27E8}U+202E\u{27E9}y.rs"), "the file banner marks the override");
    assert!(has_part(&h, "\u{241B}[2J"), "the hunk heading marks the escape");
}

#[test]
fn a_closed_thread_on_a_banner_row_still_has_its_badge() {
    // An empty patch shows only a banner: a thread anchored to a line it does not show is placed there.
    let closed = json!({"key": "t", "anchor": {"start": 5, "end": 5}, "open": false, "count": 2, "child": {"type": "text", "text": "x"}});
    let (h, _) = app_with_screen(diff_view(false, json!({"patch": "", "threads": [closed]})));
    assert!(has(&h, "No changes to show."));
    assert!(has(&h, "Open thread (2)"), "the badge is drawn although the row is a banner");
}
