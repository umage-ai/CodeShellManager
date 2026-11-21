//! `code` and `diff` nodes: parsing, caps, hostile text, patches.

use serde_json::{Value, json};

use super::*;

const ADDON: &str = "demo";

fn render(root: Value) -> Result<ScreenTree, UpdateError> {
    ScreenTree::from_render(&json!({"rev": 1, "root": root}), ADDON, None)
}

fn ok(root: Value) -> ScreenTree {
    render(root).unwrap_or_else(|e| panic!("{e}"))
}

fn refused(root: Value) -> String {
    match render(root) {
        Err(UpdateError::Invalid(e)) => e.to_string(),
        other => panic!("expected a refusal, got {other:?}"),
    }
}

fn code_node(extra: Value) -> Value {
    let mut n = json!({"type": "code", "key": "c", "text": "fn a() {}\nlet x = 1;\n", "language": "rust", "path": "src/lib.rs"});
    if let (Some(o), Some(e)) = (n.as_object_mut(), extra.as_object()) {
        o.extend(e.clone());
    }
    n
}

#[test]
fn a_code_node_parses_with_marks_threads_range_actions_and_a_key_rule() {
    let t = ok(code_node(json!({
        "first_line": 10, "highlight_lines": [12, 11, 11], "scroll_to": 11,
        "marks": [{"line": 10, "tone": "danger"}, {"line": 11, "side": "old", "count": 3}, {"line": "x"}],
        "threads": [{"key": "t1", "anchor": {"start": 11, "end": 10}, "open": true, "count": 2, "child": {"type": "column", "key": "t1-body", "children": [{"type": "comment", "author": {"name": "A"}, "body": "hi"}]}}],
        "range_actions": [{"id": "comment", "label": "+ Comment"}]
    })));
    let Kind::Code(c) = &t.root.kind else { panic!() };
    assert_eq!(c.first_line, 10);
    assert_eq!(c.highlight_lines, vec![11, 12]);
    assert_eq!(c.overlay.scroll_to, Some(11));
    assert_eq!(c.overlay.marks.len(), 2, "a mark without a numeric line is dropped");
    assert_eq!(c.overlay.marks[1].count, Some(3));
    assert_eq!(c.overlay.marks[1].side, crate::diff::Side::Old);
    let th = &c.overlay.threads[0];
    assert_eq!((th.anchor.start, th.anchor.end, th.open, th.count), (10, 11, true, 2), "a reversed anchor is put in order");
    assert_eq!(c.overlay.range_actions[0].label, "+ Comment");
    let CodeContent::Text(doc) = &c.content else { panic!() };
    assert_eq!(doc.len(), 2);
    assert!(t.find("t1-body").is_some(), "a thread's child is in the tree, so a patch can replace it");
    // Threads or range actions mean events, so the node needs a key.
    let mut n = code_node(json!({"range_actions": [{"id": "c"}]}));
    n.as_object_mut().unwrap().remove("key");
    assert!(refused(n).contains("needs a `key`"));
    let mut n = code_node(json!({}));
    n.as_object_mut().unwrap().remove("key");
    assert!(render(n).is_ok(), "a code node that sends nothing needs no key");
}

#[test]
fn thread_keys_share_the_key_space_with_nodes() {
    let thread = |k: &str| json!({"key": k, "anchor": {"start": 1, "end": 1}, "child": {"type": "text", "text": "x"}});
    assert!(refused(code_node(json!({"threads": [thread("a"), thread("a")]}))).contains("key `a`"));
    assert!(refused(json!({"type": "column", "children": [code_node(json!({"threads": [thread("c")]}))]})).contains("key `c`"), "the same as the node's own key");
    assert!(refused(code_node(json!({"threads": [{"key": "", "anchor": {}, "child": {"type": "text"}}]}))).contains("key"));
}

#[test]
fn over_cap_overlays_are_refused_and_big_text_is_withheld_not_fatal() {
    let marks: Vec<Value> = (1..=(code::MAX_MARKS as u64 + 1)).map(|i| json!({"line": i})).collect();
    assert!(refused(code_node(json!({"marks": marks}))).contains("marks"));
    let threads: Vec<Value> = (0..=code::MAX_THREADS).map(|i| json!({"key": format!("t{i}"), "anchor": {"start": 1}, "child": {"type": "spacer"}})).collect();
    assert!(refused(code_node(json!({"threads": threads}))).contains("threads"));
    let ra: Vec<Value> = (0..=code::MAX_RANGE_ACTIONS).map(|i| json!({"id": format!("a{i}")})).collect();
    assert!(refused(code_node(json!({"range_actions": ra}))).contains("range actions"));
    assert!(refused(code_node(json!({"range_actions": [{"id": "a"}, {"id": "a"}]}))).contains("two range actions"));
    assert!(refused(code_node(json!({"marks": "x"}))).contains("array"));
    assert!(refused(code_node(json!({"text": 5}))).contains("string"));
    // A thread's child counts toward the node cap, so threads cannot smuggle a huge tree.
    let kids: Vec<Value> = (0..MAX_NODES).map(|_| json!({"type": "spacer"})).collect();
    assert!(refused(code_node(json!({"threads": [{"key": "t", "anchor": {"start": 1}, "child": {"type": "column", "children": kids}}]}))).contains("nodes"));

    // Text over 512 KiB: the node says so, the tree stays valid. (A whole message over 768 KiB
    // is still refused by the tree cap.)
    let big = "x".repeat(code::MAX_CODE_BYTES + 1);
    let t = ok(code_node(json!({"text": big})));
    let Kind::Code(c) = &t.root.kind else { panic!() };
    let CodeContent::Withheld(w) = &c.content else { panic!("{:?}", c.content) };
    assert!(w.0.contains("too large"), "{}", w.0);
    let t = ok(json!({"type": "diff", "patch": "x".repeat(code::MAX_CODE_BYTES + 1)}));
    let Kind::Diff(d) = &t.root.kind else { panic!() };
    assert!(matches!(&d.content, DiffContent::Withheld(w) if w.0.contains("too large") && w.0.contains("Open the file")));
    let t = ok(code_node(json!({"text": null, "blob": {"id": "b1", "lines": 48210, "bytes": 3301120}})));
    let Kind::Code(c) = &t.root.kind else { panic!() };
    assert!(matches!(&c.content, CodeContent::Withheld(w) if w.0.contains("48210 lines")));
}

#[test]
fn a_diff_node_parses_its_patch_and_view() {
    let patch = "--- a/f\n+++ b/f\n@@ -1 +1 @@\n-a\n+b\n";
    let t = ok(json!({"type": "diff", "key": "d", "patch": patch, "view": "split", "language": "rust", "marks": [{"line": 1}]}));
    let Kind::Diff(d) = &t.root.kind else { panic!() };
    assert!(d.split);
    let DiffContent::Patch(p) = &d.content else { panic!() };
    assert_eq!(p.files.len(), 1);
    let t = ok(json!({"type": "diff", "patch": patch}));
    let Kind::Diff(d) = &t.root.kind else { panic!() };
    assert!(!d.split, "unified is the default");
    assert!(refused(json!({"type": "diff", "patch": 3})).contains("string"));
}

#[test]
fn hostile_code_and_diff_text_is_kept_not_interpreted() {
    let evil = "a\u{1b}[2J\u{202e}b\r\n\u{0}\u{7f}\u{200b}c";
    let t = ok(code_node(json!({"text": evil})));
    let Kind::Code(c) = &t.root.kind else { panic!() };
    let CodeContent::Text(d) = &c.content else { panic!() };
    assert_eq!(d.line(0), "a\u{1b}[2J\u{202e}b", "the renderer marks these; the model keeps them");
    // A patch of garbage and a patch of binary-looking text both parse to something drawable.
    for patch in ["\u{0}\u{1}\u{2}".repeat(100), "@@ -1 +1 @@\n\u{1b}[31m\n+x".into(), "diff --git a b\nBinary files a and b differ\n".into()] {
        assert!(render(json!({"type": "diff", "patch": patch})).is_ok());
    }
}

#[test]
fn a_patch_can_replace_a_thread_child_but_not_remove_it() {
    let t = ok(code_node(json!({"threads": [{"key": "t1", "open": true, "anchor": {"start": 1}, "child": {"type": "text", "key": "body", "text": "one"}}]})));
    let patched = t
        .apply_patch(&json!({"rev": 2, "base": 1, "ops": [{"op": "replace", "key": "body", "node": {"type": "text", "key": "body", "text": "two"}}]}), ADDON)
        .unwrap_or_else(|e| panic!("{e}"));
    let Kind::Text { text, .. } = &patched.find("body").unwrap().kind else { panic!() };
    assert_eq!(text, "two");
    assert!(t.apply_patch(&json!({"rev": 2, "base": 1, "ops": [{"op": "remove", "key": "body"}]}), ADDON).is_err());
}

#[test]
fn code_with_text_at_the_size_cap_still_parses_quickly() {
    let line = "let x = 1; // some code\n";
    let text = line.repeat(code::MAX_CODE_BYTES / line.len());
    let started = std::time::Instant::now();
    let t = ok(code_node(json!({"text": text})));
    assert!(started.elapsed() < std::time::Duration::from_secs(5));
    let Kind::Code(c) = &t.root.kind else { panic!() };
    assert!(matches!(&c.content, CodeContent::Text(d) if d.len() > 20_000));
}
