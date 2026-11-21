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

fn nest(levels: usize) -> Value {
    let mut v = json!({"type": "text", "text": "leaf"});
    for _ in 1..levels {
        v = json!({"type": "column", "children": [v]});
    }
    v
}

#[test]
fn a_tree_with_every_widget_parses() {
    let t = ok(json!({"type": "column", "key": "root", "gap": "md", "children": [
        {"type": "toolbar", "children": [{"type": "segmented", "key": "src", "options": [{"value": "w", "label": "Working"}, "Staged"], "selected": "w"}]},
        {"type": "split", "ratio": 0.3, "children": [
            {"type": "file_list", "key": "files", "view": "tree", "selected": "src/a.rs", "files": [{"path": "src\\a.rs", "status": "M"}, {"path": "b.md", "status": "Z"}]},
            {"type": "scroll", "child": {"type": "column", "children": [
                {"type": "section", "key": "s", "title": "Threads", "collapsible": true, "children": [{"type": "text", "text": "hi", "tone": "danger", "style": "mono"}]},
                {"type": "markdown", "source": "# Hi\n\n**x**"},
                {"type": "comment", "author": {"name": "Claude", "kind": "agent"}, "time_ms": 5, "body": "done", "badges": ["new"], "actions": [{"type": "button", "label": "Ok", "event": true, "key": "ok"}]},
                {"type": "badge", "text": "3", "tone": "info"}, {"type": "icon", "name": "branch"},
                {"type": "kv", "rows": [["a", "b"], ["bad"]]}, {"type": "empty", "title": "None"},
                {"type": "progress", "value": 3.0, "label": "x"}, {"type": "divider"}, {"type": "spacer"},
                {"type": "text_input", "key": "in", "value": "abcdef", "max_len": 3, "multiline": true},
                {"type": "checkbox", "key": "c", "label": "C", "checked": true},
                {"type": "select", "key": "sel", "options": ["a", "b"], "selected": "zzz"},
                {"type": "list", "key": "l", "items": [{"key": "1", "title": "One", "badges": [{"text": "x", "tone": "success"}]}], "selected": "1"},
                {"type": "tabs", "key": "tabs", "tabs": [{"key": "a", "label": "A"}, {"key": "b", "label": "B", "badge": "2"}], "selected": "b", "child": {"type": "text", "text": "tab b"}}
            ]}}
        ]}
    ]}));
    assert_eq!(t.rev, 1);
    assert_eq!(t.nodes, 25);
    let Kind::FileList { files, tree, selected } = &t.find("files").unwrap().kind else { panic!() };
    assert_eq!(files.len(), 2);
    assert_eq!(files[0].path, "src/a.rs", "backslashes normalised");
    assert_eq!(files[1].status, None, "an unknown status is dropped");
    assert!(*tree);
    assert_eq!(selected.as_deref(), Some("src/a.rs"));
    let Kind::TextInput { value, max_len, .. } = &t.find("in").unwrap().kind else { panic!() };
    assert_eq!((value.as_str(), *max_len), ("abc", 3), "the value is cut to max_len");
    let Kind::Select { selected, .. } = &t.find("sel").unwrap().kind else { panic!() };
    assert_eq!(selected, "", "a selection that is not an option is dropped");
    let Kind::Tabs { selected, .. } = &t.find("tabs").unwrap().kind else { panic!() };
    assert_eq!(selected, "b");
    let Kind::Split { ratio, .. } = &t.root.children()[1].kind else { panic!() };
    assert!((ratio - 0.3).abs() < 1e-6);
    assert!(t.find("ok").is_some(), "a comment's action buttons are in the tree");
}

#[test]
fn depth_is_capped() {
    assert!(render(nest(MAX_DEPTH)).is_ok());
    assert!(refused(nest(MAX_DEPTH + 1)).contains("deeper"));
    // Far deeper than serde_json itself would parse is refused before we recurse.
    assert!(refused(nest(MAX_DEPTH + 60)).contains("deeper"));
}

#[test]
fn node_count_is_capped() {
    let kids = |n: usize| (0..n).map(|_| json!({"type": "spacer"})).collect::<Vec<_>>();
    assert!(render(json!({"type": "column", "children": kids(MAX_NODES - 1)})).is_ok());
    assert!(refused(json!({"type": "column", "children": kids(MAX_NODES)})).contains("nodes"));
}

#[test]
fn keys_must_be_valid_and_unique() {
    let col = |a: Value, b: Value| json!({"type": "column", "children": [a, b]});
    let t = |k: Value| json!({"type": "text", "text": "x", "key": k});
    assert!(refused(col(t(json!("a")), t(json!("a")))).contains("two nodes have the key"));
    assert!(refused(t(json!(""))).contains("key"));
    assert!(refused(t(json!("k".repeat(MAX_KEY + 1)))).contains("key"));
    assert!(refused(t(json!("a\u{1b}b"))).contains("key"));
    assert!(refused(t(json!(5))).contains("string"));
    assert!(render(t(json!("k".repeat(MAX_KEY)))).is_ok());
    // A node that sends events needs one.
    for ty in ["text_input", "checkbox", "select", "segmented", "list", "file_list", "tabs"] {
        assert!(refused(json!({"type": ty})).contains("needs a `key`"), "{ty}");
    }
    assert!(refused(json!({"type": "button", "label": "x", "event": true})).contains("needs a `key`"));
    assert!(render(json!({"type": "button", "label": "x", "action": {"tool": "demo.go"}})).is_ok(), "an action button needs no key");
    // Duplicates inside a list or tabs.
    assert!(refused(json!({"type": "list", "key": "l", "items": [{"key": "1"}, {"key": "1"}]})).contains("two list items"));
    assert!(refused(json!({"type": "list", "key": "l", "items": [{"title": "no key"}]})).contains("`key`"));
}

#[test]
fn strings_are_capped_and_labels_are_cleaned() {
    assert!(refused(json!({"type": "text", "text": "x".repeat(MAX_STRING + 1)})).contains("16 KiB"));
    assert!(render(json!({"type": "text", "text": "x".repeat(MAX_STRING)})).is_ok());
    assert!(refused(json!({"type": "markdown", "source": "x".repeat(markdown::MAX_SOURCE + 1)})).contains("Markdown"));
    assert!(refused(json!({"type": "empty", "body": "x".repeat(MAX_STRING + 1)})).contains("body"));
    let t = ok(json!({"type": "badge", "text": format!("a\u{202E}b{}", "z".repeat(500))}));
    let Kind::Badge(b) = &t.root.kind else { panic!() };
    assert_eq!(b.text.chars().count(), MAX_LABEL);
    assert!(!b.text.contains('\u{202E}'));
    // Body text keeps its hostile characters for the renderer to reveal.
    let t = ok(json!({"type": "text", "text": "a\u{202E}b"}));
    let Kind::Text { text, .. } = &t.root.kind else { panic!() };
    assert!(text.contains('\u{202E}'));
    assert!(reveal(text).iter().any(|s| s.marker));
}

#[test]
fn the_serialized_size_is_capped() {
    // Many 16 KiB strings: over the node-free size cap well before the node cap.
    let kids: Vec<Value> = (0..60).map(|_| json!({"type": "text", "text": "y".repeat(MAX_STRING)})).collect();
    let r = refused(json!({"type": "column", "children": kids}));
    assert!(r.contains("KiB"), "{r}");
}

#[test]
fn unknown_types_degrade_and_odd_shapes_are_refused() {
    let t = ok(json!({"type": "hologram", "key": "h", "children": [{"type": "text"}]}));
    assert!(matches!(&t.root.kind, Kind::Unsupported { type_name } if type_name == "hologram"));
    assert_eq!(t.nodes, 1, "an unsupported node's children are not parsed");
    assert!(matches!(ok(json!({"type": "hologram", "patch": ""})).root.kind, Kind::Unsupported { .. }));
    for bad in [json!(5), json!("x"), json!([]), json!(null), json!({}), json!({"type": 4}), json!({"type": "column", "children": 5}), json!({"type": "column", "children": [1]})] {
        assert!(matches!(render(bad.clone()), Err(UpdateError::Invalid(_))), "{bad}");
    }
    assert!(refused(json!({"type": "split", "children": [{"type": "spacer"}]})).contains("two children"));
    assert!(refused(json!({"type": "scroll"})).contains("child"));
    assert!(refused(json!({"type": "comment", "actions": [{"type": "text"}]})).contains("buttons"));
    assert!(refused(json!({"type": "button", "event": true, "key": "b", "action": {"tool": "x"}})).contains("both"));
}

#[test]
fn render_envelope_rules() {
    assert!(matches!(ScreenTree::from_render(&json!({"root": {"type": "spacer"}}), ADDON, None), Err(UpdateError::Invalid(_))));
    assert!(matches!(ScreenTree::from_render(&json!({"rev": 1}), ADDON, None), Err(UpdateError::Invalid(_))));
    assert!(matches!(ScreenTree::from_render(&json!({"rev": "1", "root": {"type": "spacer"}}), ADDON, None), Err(UpdateError::Invalid(_))));
    assert!(matches!(ScreenTree::from_render(&json!(null), ADDON, None), Err(UpdateError::Invalid(_))));
    let r = |rev: u64, have: Option<u64>| ScreenTree::from_render(&json!({"rev": rev, "root": {"type": "spacer"}}), ADDON, have);
    assert_eq!(r(3, Some(3)).unwrap_err(), UpdateError::Stale { have: 3, got: 3 });
    assert_eq!(r(2, Some(3)).unwrap_err(), UpdateError::Stale { have: 3, got: 2 });
    assert!(r(4, Some(3)).is_ok());
}

#[test]
fn action_buttons_get_a_policy_from_the_tool_id() {
    let p = |tool: &str| action_policy(ADDON, tool);
    assert_eq!(p("demo.review_open"), ActionPolicy::Direct, "the add-on's own tool");
    assert!(matches!(p("other.x"), ActionPolicy::Refused(_)), "another add-on's tool");
    for t in ["session_focus", "screen_open", "screen_close", "inspector_show"] {
        assert_eq!(p(t), ActionPolicy::Direct, "{t}");
    }
    for t in ["session_send_text", "open_url", "open_with"] {
        assert_eq!(p(t), ActionPolicy::Confirm, "{t}");
    }
    for t in ["addon_enable", "addon_secret_get", "plugin_restart", "consent_answer", "settings_set", "session_close", "state_import", "app_quit", "session_send_text_many", "session_new", "task_new", "x", ""] {
        assert!(matches!(p(t), ActionPolicy::Refused(_)), "{t}");
    }
    let t = ok(json!({"type": "button", "label": "Send", "action": {"tool": "session_send_text", "params": {"session_id": "s1", "text": "hi"}}}));
    let Kind::Button(b) = &t.root.kind else { panic!() };
    let Bind::Action(a) = &b.bind else { panic!() };
    assert_eq!((a.tool.as_str(), &a.policy), ("session_send_text", &ActionPolicy::Confirm));
    assert_eq!(a.params["text"], "hi");
    // Bad actions refuse the tree.
    assert!(refused(json!({"type": "button", "action": {"tool": "has space"}})).contains("tool id"));
    assert!(refused(json!({"type": "button", "action": {"tool": "x", "params": [1]}})).contains("object"));
    assert!(refused(json!({"type": "button", "action": {"tool": "x", "params": {"a": "y".repeat(MAX_ACTION_PARAMS)}}})).contains("params"));
    assert!(refused(json!({"type": "button", "action": 5})).contains("object"));
    assert!(refused(json!({"type": "button", "action": {}})).contains("`tool`"));
}

fn base_tree() -> ScreenTree {
    ok(json!({"type": "column", "key": "root", "children": [
        {"type": "text", "key": "a", "text": "one"},
        {"type": "section", "key": "sec", "title": "S", "children": [{"type": "text", "key": "b", "text": "two"}]},
        {"type": "split", "key": "sp", "children": [{"type": "text", "key": "l", "text": "l"}, {"type": "text", "key": "r", "text": "r"}]}
    ]}))
}

fn patch(rev: u64, base: u64, ops: Value) -> Value {
    json!({"rev": rev, "base": base, "ops": ops})
}

#[test]
fn patches_replace_remove_and_append_by_key() {
    let t = base_tree();
    let p = t
        .apply_patch(
            &patch(2, 1, json!([
                {"op": "replace", "key": "a", "node": {"type": "text", "key": "a", "text": "ONE"}},
                {"op": "append", "key": "sec", "node": {"type": "text", "key": "c", "text": "three"}},
                {"op": "remove", "key": "b"}
            ])),
            ADDON,
        )
        .unwrap();
    assert_eq!(p.rev, 2);
    assert!(matches!(&p.find("a").unwrap().kind, Kind::Text { text, .. } if text == "ONE"));
    assert!(p.find("b").is_none() && p.find("c").is_some());
    assert_eq!(p.nodes, 7 - 1 + 1);
    assert!(matches!(&t.find("a").unwrap().kind, Kind::Text { text, .. } if text == "one"), "the old tree is untouched");
    // Replacing a node deep inside a split.
    let p = t.apply_patch(&patch(2, 1, json!([{"op": "replace", "key": "r", "node": {"type": "badge", "key": "r", "text": "x"}}])), ADDON).unwrap();
    assert!(matches!(&p.find("r").unwrap().kind, Kind::Badge(_)));
}

#[test]
fn patch_revision_rules() {
    let t = base_tree();
    let noop = json!([]);
    assert_eq!(t.apply_patch(&patch(1, 1, noop.clone()), ADDON).unwrap_err(), UpdateError::Stale { have: 1, got: 1 });
    assert_eq!(t.apply_patch(&patch(0, 1, noop.clone()), ADDON).unwrap_err(), UpdateError::Stale { have: 1, got: 0 });
    assert_eq!(t.apply_patch(&patch(5, 4, noop.clone()), ADDON).unwrap_err(), UpdateError::BaseMismatch { have: 1, base: 4 });
    assert!(t.apply_patch(&patch(2, 1, noop), ADDON).is_ok());
    for bad in [json!({"base": 1, "ops": []}), json!({"rev": 2, "ops": []}), json!({"rev": 2, "base": 1}), json!({"rev": 2, "base": 1, "ops": 5}), json!(7)] {
        assert!(matches!(t.apply_patch(&bad, ADDON), Err(UpdateError::Invalid(_))), "{bad}");
    }
}

#[test]
fn hostile_patches_are_refused_whole() {
    let t = base_tree();
    let bad = |ops: Value| match t.apply_patch(&patch(2, 1, ops), ADDON) {
        Err(UpdateError::Invalid(e)) => e.to_string(),
        other => panic!("{other:?}"),
    };
    assert!(bad(json!([{"op": "remove", "key": "nope"}])).contains("no node"));
    assert!(bad(json!([{"op": "replace", "key": "nope", "node": {"type": "spacer"}}])).contains("no node"));
    assert!(bad(json!([{"op": "remove", "key": "root"}])).contains("root"));
    assert!(bad(json!([{"op": "remove", "key": "l"}])).contains("cannot be removed"), "a split keeps its two children");
    assert!(bad(json!([{"op": "append", "key": "a", "node": {"type": "spacer"}}])).contains("cannot take"));
    assert!(bad(json!([{"op": "append", "key": "sp", "node": {"type": "spacer"}}])).contains("cannot take"));
    assert!(bad(json!([{"op": "frobnicate", "key": "a"}])).contains("not a patch operation"));
    assert!(bad(json!([{"op": "replace", "key": "a"}])).contains("`node`"));
    assert!(bad(json!([{"op": "replace"}])).contains("`key`"));
    assert!(bad(json!([5])).contains("object"));
    // A replacement that breaks the key rule is refused after the patch is applied.
    assert!(bad(json!([{"op": "replace", "key": "a", "node": {"type": "text", "key": "b", "text": "dup"}}])).contains("two nodes have the key"));
    // A replacement that is too deep, or that pushes the tree over a cap.
    assert!(bad(json!([{"op": "replace", "key": "a", "node": nest(MAX_DEPTH)}])).contains("deeper"));
    let many: Vec<Value> = (0..MAX_NODES).map(|_| json!({"type": "spacer"})).collect();
    assert!(bad(json!([{"op": "append", "key": "sec", "node": {"type": "column", "children": many}}])).contains("nodes"));
    let ops: Vec<Value> = (0..=MAX_PATCH_OPS).map(|_| json!({"op": "remove", "key": "nope"})).collect();
    assert!(bad(Value::Array(ops)).contains("operations"));
    // An operation that fails leaves no half-applied tree: the result is an error, and the input is intact.
    assert!(t.find("a").is_some());
}

#[test]
fn parsing_never_panics_on_random_json() {
    // A tiny xorshift: deterministic, no dependency.
    struct Rng(u64);
    impl Rng {
        fn next(&mut self) -> u64 {
            self.0 ^= self.0 << 13;
            self.0 ^= self.0 >> 7;
            self.0 ^= self.0 << 17;
            self.0
        }
        fn pick<'a, T>(&mut self, s: &'a [T]) -> &'a T {
            &s[(self.next() % s.len() as u64) as usize]
        }
    }
    const TYPES: &[&str] = &["column", "row", "split", "scroll", "tabs", "section", "text", "markdown", "button", "text_input", "list", "file_list", "comment", "select", "kv", "progress", "x", ""];
    const KEYS: &[&str] = &["type", "key", "children", "child", "text", "items", "files", "tabs", "options", "rows", "action", "event", "value", "source", "body", "actions", "badges", "author", "selected", "max_len", "ratio"];
    fn rand_json(r: &mut Rng, depth: usize) -> Value {
        match r.next() % 9 {
            0 => Value::Null,
            1 => json!(r.next() as i64),
            2 => json!(0.25 + (r.next() % 100) as f64 / 50.0),
            3 => Value::Bool(r.next().is_multiple_of(2)),
            4 => Value::String((0..(r.next() % 12)).map(|_| char::from_u32((r.next() % 0x3000) as u32).unwrap_or('x')).collect()),
            5 if depth < 6 => Value::Array((0..(r.next() % 4)).map(|_| rand_json(r, depth + 1)).collect()),
            _ if depth < 6 => {
                let mut m = serde_json::Map::new();
                m.insert("type".into(), json!(*r.pick(TYPES)));
                for _ in 0..(r.next() % 5) {
                    m.insert((*r.pick(KEYS)).to_owned(), rand_json(r, depth + 1));
                }
                Value::Object(m)
            }
            _ => Value::Null,
        }
    }
    let mut r = Rng(0x9E37_79B9_7F4A_7C15);
    let mut parsed = 0;
    for _ in 0..4000 {
        let root = rand_json(&mut r, 0);
        if let Ok(t) = ScreenTree::from_render(&json!({"rev": 1, "root": root.clone()}), ADDON, None) {
            parsed += 1;
            // Whatever parses can be patched with random ops without a panic.
            let ops = json!([{"op": "replace", "key": "key", "node": rand_json(&mut r, 3)}, {"op": "remove", "key": "x"}, rand_json(&mut r, 4)]);
            let _ = t.apply_patch(&patch(2, 1, ops), ADDON);
        }
        let _ = ScreenTree::from_render(&root, ADDON, Some(0));
        let _ = ScreenTree::from_render(&json!({"rev": 1, "root": nest(40)}), ADDON, None);
    }
    assert!(parsed > 100, "the generator should make some valid trees ({parsed})");
}

#[test]
fn declared_screens_are_validated() {
    let (s, p) = parse_screens(
        Some(&json!([
            {"id": "demo.main", "title": "Main \u{202E}", "icon": "branch!", "scope": "project", "summary": "s"},
            {"id": "demo.main", "title": "Dup"},
            {"id": "other.x", "title": "Not mine"},
            {"id": "demo.Bad", "title": "Bad name"}
        ])),
        ADDON,
    );
    assert_eq!(s.iter().map(|d| d.id.as_str()).collect::<Vec<_>>(), vec!["demo.main"]);
    assert_eq!(s[0].title, "Main");
    assert_eq!(s[0].icon, "branch");
    assert_eq!(s[0].scope, ScreenScope::Project);
    assert_eq!(p.len(), 3, "{p:?}");
    let (s, p) = parse_screens(Some(&json!([{"id": "demo.notitle"}, "nope", {"id": "demo.b", "title": "B"}])), ADDON);
    assert_eq!((s.len(), p.len()), (1, 2));
    assert_eq!(s[0].scope, ScreenScope::Global);
    assert!(parse_screens(None, ADDON).0.is_empty());
    assert!(!parse_screens(Some(&json!(5)), ADDON).1.is_empty());
    assert!(parse_screens(Some(&json!(5)), ADDON).0.is_empty());
    // More than the cap.
    let many: Vec<Value> = (0..9).map(|i| json!({"id": format!("demo.s{i}"), "title": "S"})).collect();
    let (s, p) = parse_screens(Some(&Value::Array(many)), ADDON);
    assert_eq!(s.len(), MAX_SCREENS);
    assert!(p.iter().any(|x| x.contains("ignored")));
}

#[test]
fn context_caps() {
    assert!(ScreenContext::default().validate().is_ok());
    let c = ScreenContext { path: Some("x".repeat(5000)), ..ScreenContext::default() };
    assert!(c.validate().is_err());
    let c = ScreenContext { session_id: Some("a\nb".into()), ..ScreenContext::default() };
    assert!(c.validate().is_err());
    let c = ScreenContext { args: Some(json!({"a": "y".repeat(MAX_CONTEXT_ARGS)})), ..ScreenContext::default() };
    assert!(c.validate().is_err());
    let c = ScreenContext { args: Some(json!({"a": 1})), project: Some("repo:x".into()), ..ScreenContext::default() };
    assert!(c.validate().is_ok());
}

#[test]
fn events_become_notification_params() {
    let e = ScreenEvent::new("files", EventKind::Select, json!({"path": "src/lib.rs"}));
    assert_eq!(e.params("i7", 6), json!({"instance": "i7", "rev": 6, "key": "files", "kind": "select", "value": {"path": "src/lib.rs"}}));
    assert!(EventKind::Change.coalesces() && EventKind::Visible.coalesces());
    assert!(!EventKind::Click.coalesces() && !EventKind::Submit.coalesces());
}

#[test]
fn the_render_rate_gate_lets_thirty_a_second_through() {
    let mut g = RateGate::new();
    assert!(g.admit(1_000));
    assert!(!g.admit(1_010));
    assert!(!g.admit(1_032));
    assert!(g.admit(1_033));
    assert!(!g.admit(1_040));
    assert!(g.admit(2_000));
    // A clock that went backwards does not wedge it.
    assert!(g.admit(10));
    let admitted = (0..1000u64).filter(|t| RateGate::new().admit(*t)).count();
    assert_eq!(admitted, 1000, "fresh gates always admit");
    let mut g = RateGate::new();
    assert_eq!((0..1000u64).filter(|t| g.admit(*t)).count(), 31, "about 30 a second");
}
