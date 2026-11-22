//! The engine's read model converts to the owned wire types, and the wire JSON is the engine's
//! JSON: every field the wire carries has the same value at the same path. Runs on a fake PTY.
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

mod common;

use common::H;
use csm_peer_proto::{Snapshot as WireSnapshot, Status};
use serde_json::{Value, json};

/// Every key the wire object has must be in the engine object with an equal value. The wire may
/// omit engine fields (the look, the subagent report, add-on summaries); it may not invent or
/// change one.
fn assert_wire_is_engine_subset(path: &str, wire: &Value, engine: &Value) {
    match (wire, engine) {
        (Value::Object(w), Value::Object(e)) => {
            for (k, wv) in w {
                let ev = e.get(k).unwrap_or_else(|| panic!("{path}.{k}: the engine JSON has no such field"));
                assert_wire_is_engine_subset(&format!("{path}.{k}"), wv, ev);
            }
        }
        (Value::Array(w), Value::Array(e)) => {
            assert_eq!(w.len(), e.len(), "{path}: length");
            for (i, (wv, ev)) in w.iter().zip(e).enumerate() {
                assert_wire_is_engine_subset(&format!("{path}[{i}]"), wv, ev);
            }
        }
        _ => assert_eq!(wire, engine, "{path}"),
    }
}

#[test]
fn an_engine_snapshot_converts_to_the_wire_with_the_engine_json() {
    let h = H::new();
    let claude = h.session("claude");
    let shell = h.session("pwsh");
    h.ok("group_new", json!({"name": "Work"}));
    let snap = h.engine.view();

    let wire = WireSnapshot::from(&*snap);

    assert_eq!(wire.sessions.len(), 2);
    let first = wire.session(&claude).expect("the claude session is on the wire");
    assert_eq!(first.kind, "local");
    assert_eq!(first.status, Status::Idle);
    assert_eq!(first.status_label, "Idle");
    assert!(first.is_claude, "the claude command is a Claude session");
    assert!(wire.session(&shell).is_some());
    assert_eq!(wire.groups.len(), 1);
    assert_eq!(wire.groups[0].name, "Work");
    assert_eq!(wire.version, snap.version);

    let engine_json = serde_json::to_value(&*snap).unwrap();
    let wire_json = serde_json::to_value(&wire).unwrap();
    assert_wire_is_engine_subset("snapshot", &wire_json, &engine_json);
}

#[test]
fn the_wire_snapshot_reads_back_unchanged() {
    let h = H::new();
    h.session("claude");
    h.session("pwsh");
    h.ok("group_new", json!({"name": "Work"}));
    let wire = WireSnapshot::from(&*h.engine.view());

    let text = serde_json::to_string(&wire).unwrap();
    let back: WireSnapshot = serde_json::from_str(&text).unwrap();
    assert_eq!(back, wire);
}
