#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
//! The `qr` node: parsing, the text cap, hostile text, and that a bad text never breaks the tree.

use serde_json::{Value, json};

use super::*;
use crate::qr::MAX_QR_TEXT;

const ADDON: &str = "demo";

fn render(root: Value) -> Result<ScreenTree, UpdateError> {
    ScreenTree::from_render(&json!({"rev": 1, "root": root}), ADDON, None)
}

fn qr_of(root: Value) -> (Option<Matrix>, QrSize, String) {
    let t = render(root).unwrap_or_else(|e| panic!("{e}"));
    match &t.root.kind {
        Kind::Qr { code, size, label } => (code.clone(), *size, label.clone()),
        other => panic!("expected a qr node, got {other:?}"),
    }
}

#[test]
fn a_good_qr_node_parses_with_its_matrix_and_size() {
    let (code, size, label) = qr_of(json!({"type": "qr", "text": "https://example.org/pair#x", "label": "Scan <me>", "size": "lg"}));
    let code = code.expect("a good text has a code");
    assert!(code.size() >= 21 && code.is_dark(0, 0));
    assert_eq!(size, QrSize::Lg);
    assert_eq!(label, "Scan <me>", "the label is plain text");
    let (_, default, _) = qr_of(json!({"type": "qr", "text": "x"}));
    assert_eq!(default, QrSize::Md, "medium is the default");
    let (_, small, _) = qr_of(json!({"type": "qr", "text": "x", "size": "sm"}));
    assert_eq!(small, QrSize::Sm);
    let (_, odd, _) = qr_of(json!({"type": "qr", "text": "x", "size": "huge"}));
    assert_eq!(odd, QrSize::Md, "an unknown size is the default");
}

#[test]
fn the_worst_case_relay_link_is_accepted_at_the_cap() {
    let link = format!("https://phone.example.org/v/0.1.0/#p={}", "A".repeat(MAX_QR_TEXT - 40));
    assert!(link.len() <= MAX_QR_TEXT);
    let (code, _, _) = qr_of(json!({"type": "qr", "text": link}));
    assert!(code.is_some(), "a text at the cap draws");
}

#[test]
fn a_bad_text_is_a_notice_not_a_broken_tree() {
    let oversize = "a".repeat(MAX_QR_TEXT + 1);
    let hostile = ["", "line\nbreak", "nul\u{0}", "bell\u{7}", "tab\t"];
    let (code, _, _) = qr_of(json!({"type": "qr", "text": oversize}));
    assert!(code.is_none(), "oversize draws no code");
    for t in hostile {
        let (code, _, _) = qr_of(json!({"type": "qr", "text": t}));
        assert!(code.is_none(), "{t:?} draws no code");
    }
    // A missing text, a number and a list are bad props too.
    for v in [json!({"type": "qr"}), json!({"type": "qr", "text": 7}), json!({"type": "qr", "text": ["a"]})] {
        let (code, _, _) = qr_of(v);
        assert!(code.is_none());
    }
}

#[test]
fn a_qr_node_beside_other_nodes_leaves_them_intact() {
    let t = render(json!({"type": "column", "children": [
        {"type": "qr", "text": "a\u{0}b"},
        {"type": "text", "text": "still here"},
        {"type": "qr", "text": "https://example.org/ok", "key": "ok"},
    ]}))
    .unwrap_or_else(|e| panic!("{e}"));
    let kids = t.root.children();
    assert_eq!(kids.len(), 3);
    assert!(matches!(kids[0].kind, Kind::Qr { code: None, .. }));
    assert!(matches!(kids[2].kind, Kind::Qr { code: Some(_), .. }));
    assert_eq!(kids[0].type_name(), "qr");
}

#[test]
fn a_wrong_shaped_node_is_still_refused_by_the_tree_rules() {
    // The tree's own rules stay: a node that is not an object, or with no type, is refused.
    assert!(render(json!({"type": "column", "children": ["qr"]})).is_err());
    assert!(render(json!({"text": "no type"})).is_err());
}
