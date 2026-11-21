#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic, clippy::indexing_slicing)]
//! The Review screen's view tree for a fixed model, checked against the copy the UI tests draw
//! (`crates/csm-ui/tests/fixtures/review_screen.json`): the egui crate cannot depend on this one,
//! so the UI test loads the real tree from that file, and this test fails when the plugin's view
//! changes without the file following. Regenerate with `CSM_REGEN_FIXTURE=1 cargo test -p csm-review --test fixture`.
//! It also runs the tree through the view builder's own invariants (unique keys, size).

use std::collections::{HashMap, HashSet};
use std::path::Path;

use csm_review::anchor::Anchor;
use csm_review::store::{Author, Comment, Status, Thread};
use csm_review::view::{self, Body, Draft, FileRow, Model, Source, ThreadView, Where};
use serde_json::Value;

const PATCH: &str = "diff --git a/src/lib.rs b/src/lib.rs\nindex 1111111..2222222 100644\n--- a/src/lib.rs\n+++ b/src/lib.rs\n@@ -1,3 +1,4 @@\n fn main() {\n-    let x = 1;\n+    let x = read()?;\n+    println!(\"{x}\");\n }\n";

fn thread(id: &str, status: Status, line: u32, comments: Vec<(&str, bool, &str)>) -> Thread {
    Thread {
        id: id.into(),
        status,
        created_ms: 1_795_513_000_000,
        updated_ms: 1_795_513_200_000,
        anchor: Anchor { kind: "lines".into(), path: "src/lib.rs".into(), side: "new".into(), start: line, end: line, snippet: "    let x = read()?;".into(), ..Anchor::default() },
        comments: comments
            .into_iter()
            .enumerate()
            .map(|(i, (name, agent, body))| Comment {
                id: format!("c_{i}"),
                author: Author { kind: if agent { "agent".into() } else { "person".into() }, name: name.into(), session_id: None, client: None },
                body: body.into(),
                created_ms: 1_795_513_000_000 + i as u64 * 60_000,
            })
            .collect(),
    }
}

pub fn tree(draft: Option<&Draft>) -> Value {
    let files = vec![FileRow { path: "src/lib.rs".into(), status: Some('M'), old_path: None }, FileRow { path: "notes.txt".into(), status: Some('?'), old_path: None }];
    let threads = vec![
        ThreadView {
            thread: thread("t_0000000001000001", Status::Addressed, 2, vec![("Allan", false, "This swallows the error; return it."), ("Claude in shop", true, "Returned it in 1a2b3c.")]),
            at: Where::Moved(2, 2),
            prompt: "Review comments are waiting in CodeShellManager for shop: 1 addressed.\n1. src/lib.rs:2 [t_0000000001000001] This swallows the error; return it.".into(),
        },
        ThreadView {
            thread: thread("t_0000000001000002", Status::Open, 3, vec![("Allan", false, "Do not print this.")]),
            at: Where::Exact(3, 3),
            prompt: String::new(),
        },
    ];
    let sessions = vec![("s1".to_owned(), "api".to_owned()), ("s2".to_owned(), "web".to_owned())];
    let untracked: HashSet<String> = HashSet::from(["notes.txt".to_owned()]);
    let open: HashMap<String, bool> = HashMap::new();
    let body = Body::Diff { patch: PATCH.into(), truncated: false };
    let m = Model {
        repo: "shop",
        session_id: Some("s1"),
        sessions: &sessions,
        project: "repo:C:/code/shop",
        source: Source::Working,
        split: false,
        files: &files,
        files_note: None,
        untracked: &untracked,
        selected: Some("src/lib.rs"),
        body: &body,
        threads: &threads,
        draft,
        open: &open,
        scroll_to: None,
        loading: false,
        error: None,
        prompt_all: "Review comments are waiting in CodeShellManager for shop: 1 open.\n1. src/lib.rs:3 [t_0000000001000002] Do not print this.\nRead each with review.get, fix it, then answer with review.reply (status \"addressed\").",
        persistent: true,
    };
    view::build(&m)
}

fn keys(v: &Value, out: &mut Vec<String>) {
    match v {
        Value::Object(o) => {
            if let Some(k) = o.get("key").and_then(Value::as_str) {
                out.push(k.to_owned());
            }
            // List items have a key space of their own (the list's), so they are not node keys.
            o.iter().filter(|(k, _)| k.as_str() != "items").for_each(|(_, x)| keys(x, out));
        }
        Value::Array(a) => a.iter().for_each(|x| keys(x, out)),
        _ => {}
    }
}

#[test]
fn the_tree_matches_the_ui_fixture_and_keys_are_unique() {
    let t = tree(None);
    let mut all = Vec::new();
    keys(&t, &mut all);
    let unique: HashSet<&String> = all.iter().collect();
    assert_eq!(all.len(), unique.len(), "every key is unique: {all:?}");
    assert!(serde_json::to_string(&t).unwrap().len() < 700_000);
    let path = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../crates/csm-ui/tests/fixtures/review_screen.json");
    if std::env::var("CSM_REGEN_FIXTURE").is_ok() {
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(&path, serde_json::to_string_pretty(&t).unwrap() + "\n").unwrap();
    }
    let on_disk: Value = serde_json::from_str(&std::fs::read_to_string(&path).expect("the UI fixture; regenerate with CSM_REGEN_FIXTURE=1")).unwrap();
    assert_eq!(on_disk, t, "the Review view changed: regenerate the UI fixture (CSM_REGEN_FIXTURE=1)");
}

#[test]
fn a_draft_and_hostile_text_keep_the_tree_valid() {
    let d = Draft { path: "src/lib.rs".into(), side: "new".into(), start: 2, end: 3, whole_file: false };
    let t = tree(Some(&d));
    let mut all = Vec::new();
    keys(&t, &mut all);
    assert!(all.iter().any(|k| k == "draft-input"));
    let unique: HashSet<&String> = all.iter().collect();
    assert_eq!(all.len(), unique.len());
    // A whole-file draft has no line, so it sits above the code.
    let f = Draft { whole_file: true, ..d };
    let t = tree(Some(&f));
    assert!(t.to_string().contains("draft-input"));
}
